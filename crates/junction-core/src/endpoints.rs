use std::{net::SocketAddr, time::Duration};

use crate::{xds, HttpResult, Trace};

#[derive(Debug, Clone)]
pub struct Retries {
    /// The HTTP error codes that retries should be applied to.
    pub codes: Vec<u16>,

    /// The total number of attempts to make when retrying this request. If
    /// unset, the client should only ever make a single request.
    pub attempts: Option<u32>,

    /// The initial amount of time to back off between requests during a series
    /// of retries. Backoff may scale up to `max_backoff` between requests at
    /// the client's discretion
    pub backoff: Option<Duration>,

    /// The maximum amount of time to back off between requests.
    pub max_backoff: Option<Duration>,
}

impl From<xds::route_configs::Retries> for Retries {
    fn from(value: xds::route_configs::Retries) -> Self {
        Self {
            codes: value.codes,
            attempts: value.attempts,
            backoff: value.backoff,
            max_backoff: value.max_backoff,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Timeouts {
    /// The total timeout for this request and all of its retries.
    pub total: Option<Duration>,

    /// The timeout for each individual request attempt. The value of this
    /// timeout should be less than or equal to the total timeout of the request.
    pub attempt: Option<Duration>,
}

impl From<xds::route_configs::Timeouts> for Timeouts {
    fn from(value: xds::route_configs::Timeouts) -> Self {
        Self {
            total: value.total,
            attempt: value.attempt,
        }
    }
}

// TODO: move to Client? all these fields can be private then.
// TODO: this is way more than just a resolved endpoint, it's the whole request
// context and the history of any retries that were made. it needs suuuuch a
// better name.
#[derive(Debug, Clone)]
pub struct Endpoint {
    // request data
    pub(crate) method: http::Method,
    pub(crate) url: crate::Url,
    pub(crate) headers: http::HeaderMap,
    pub(crate) request_hash: u64,

    // matched route info
    // TODO: do we need the matched route here???? is it enough to have the name and
    // version? is it enough to have it in the trace?
    pub(crate) cluster_name: xds::ResourceName,
    pub(crate) address: SocketAddr,
    pub(crate) previous_addrs: Vec<SocketAddr>,
    pub(crate) retries: Option<Retries>,
    pub(crate) timeouts: Option<Timeouts>,

    // FIXME: figure out what the type is here and expose it
    // pub(crate) timeouts: Option<RouteTimeouts>,
    // pub(crate) retry: Option<RouteRetry>,

    // debugging data
    pub(crate) trace: Trace,
}

impl Endpoint {
    pub fn method(&self) -> &http::Method {
        &self.method
    }

    pub fn url(&self) -> &crate::Url {
        &self.url
    }

    pub fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }

    pub fn addr(&self) -> SocketAddr {
        self.address
    }

    pub fn timeouts(&self) -> &Option<Timeouts> {
        &self.timeouts
    }

    pub fn retry(&self) -> &Option<Retries> {
        &self.retries
    }

    pub(crate) fn should_retry(&self, result: HttpResult) -> bool {
        let Some(retry) = &self.retries else {
            return false;
        };
        let Some(allowed) = &retry.attempts else {
            return false;
        };
        let allowed = *allowed as usize;

        match result {
            HttpResult::StatusError(code) if !retry.codes.contains(&code.as_u16()) => return false,
            _ => (),
        }

        // total number of attempts taken is history + 1 because we include the
        // the current addr as an attempt.
        let attempts = self.previous_addrs.len() + 1;

        attempts < allowed
    }

    // FIXME: lol
    pub fn print_trace(&self) {
        let start = self.trace.start();
        let mut phase = None;

        for event in self.trace.events() {
            if phase != Some(event.phase) {
                eprintln!("{:?}", event.phase);
                phase = Some(event.phase);
            }

            let elapsed = event.at.duration_since(start).as_secs_f64();
            eprint!("  {elapsed:.06}: {name:>16?}", name = event.kind);
            if !event.kv.is_empty() {
                eprint!(":");

                for (k, v) in &event.kv {
                    eprint!("  {k}={v}")
                }
            }
            eprintln!();
        }
    }
}

#[cfg(test)]
mod test {
    use http::StatusCode;
    use std::net::Ipv4Addr;

    use crate::{xds::ResourceName, Url};

    use super::*;

    #[test]
    fn test_endpoint_should_retry_no_policy() {
        let mut endpoint = new_endpoint();
        endpoint.retries = None;

        assert!(!endpoint.should_retry(HttpResult::StatusFailed));
        assert!(!endpoint.should_retry(HttpResult::StatusError(
            http::StatusCode::SERVICE_UNAVAILABLE
        )));
    }

    #[test]
    fn test_endpoint_should_retry_with_policy() {
        let mut endpoint = new_endpoint();
        endpoint.retries = Some(Retries {
            codes: vec![StatusCode::BAD_REQUEST.as_u16()],
            attempts: Some(3),
            backoff: Some(Duration::from_secs(2)),
            max_backoff: Some(Duration::from_secs(5)),
        });

        assert!(endpoint.should_retry(HttpResult::StatusFailed));
        assert!(endpoint.should_retry(HttpResult::StatusError(StatusCode::BAD_REQUEST)));
        assert!(!endpoint.should_retry(HttpResult::StatusError(StatusCode::SERVICE_UNAVAILABLE)));
    }

    #[test]
    fn test_endpoint_should_retry_with_history() {
        let mut endpoint = new_endpoint();
        endpoint.retries = Some(Retries {
            codes: vec![StatusCode::BAD_REQUEST.as_u16()],
            attempts: Some(3),
            backoff: Some(Duration::from_secs(2)),
            max_backoff: Some(Duration::from_secs(5)),
        });

        // first endpoint was the first attempt
        assert!(endpoint.should_retry(HttpResult::StatusFailed));
        assert!(endpoint.should_retry(HttpResult::StatusError(StatusCode::BAD_REQUEST)));
        assert!(!endpoint.should_retry(HttpResult::StatusError(StatusCode::SERVICE_UNAVAILABLE)));

        // add on ip to history - this is the second attempt
        endpoint
            .previous_addrs
            .push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 443));
        assert!(endpoint.should_retry(HttpResult::StatusFailed),);
        assert!(endpoint.should_retry(HttpResult::StatusError(StatusCode::BAD_REQUEST)),);

        // two ips in history and one current ip, three attempts have been made, shouldn't retry again
        endpoint
            .previous_addrs
            .push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 443));
        assert!(!endpoint.should_retry(HttpResult::StatusFailed));
        assert!(!endpoint.should_retry(HttpResult::StatusError(StatusCode::BAD_REQUEST)));
    }

    fn new_endpoint() -> Endpoint {
        let url: Url = "http://example.com".parse().unwrap();
        let cluster_name = ResourceName::from("example.com:443");
        let address = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 443);

        Endpoint {
            method: http::Method::GET,
            url,
            headers: Default::default(),
            cluster_name,
            address,
            request_hash: 1234,
            timeouts: None,
            retries: None,
            trace: Trace::new(),
            previous_addrs: vec![],
        }
    }
}
