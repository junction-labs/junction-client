use junction_api::{
    backend::BackendId,
    http::{RouteRetry, RouteTimeouts},
};
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use crate::{error::Trace, hash::thread_local_xxhash, HttpResult};

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

    // matched route info
    // TODO: do we need the matched route here???? is it enough to have the name and
    // version? is it enough to have it in the trace?
    pub(crate) backend: BackendId,
    pub(crate) address: SocketAddr,
    pub(crate) timeouts: Option<RouteTimeouts>,
    pub(crate) retry: Option<RouteRetry>,

    // debugging data
    pub(crate) trace: Trace,
    pub(crate) previous_addrs: Vec<SocketAddr>,
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

    pub fn timeouts(&self) -> &Option<RouteTimeouts> {
        &self.timeouts
    }

    pub fn retry(&self) -> &Option<RouteRetry> {
        &self.retry
    }

    pub(crate) fn should_retry(&self, result: HttpResult) -> bool {
        let Some(retry) = &self.retry else {
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

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Locality {
    Unknown,
    #[allow(unused)]
    Known(LocalityInfo),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LocalityInfo {
    pub(crate) region: String,
    pub(crate) zone: String,
}

/// A snapshot of endpoint data.
pub struct EndpointIter {
    endpoint_group: Arc<EndpointGroup>,
}

impl From<Arc<EndpointGroup>> for EndpointIter {
    fn from(endpoint_group: Arc<EndpointGroup>) -> Self {
        Self { endpoint_group }
    }
}

// TODO: add a way to see endpoints grouped by locality. have to decide how
// to publicly expose Locality.
impl EndpointIter {
    /// Iterate over all of the addresses in this group, without any locality
    /// information.
    pub fn addrs(&self) -> impl Iterator<Item = &SocketAddr> {
        self.endpoint_group.iter()
    }
}

#[derive(Debug, Default, Hash, PartialEq, Eq)]
pub(crate) struct EndpointGroup {
    pub(crate) hash: u64,
    endpoints: BTreeMap<Locality, Vec<SocketAddr>>,
}

impl EndpointGroup {
    pub(crate) fn new(endpoints: BTreeMap<Locality, Vec<SocketAddr>>) -> Self {
        let hash = thread_local_xxhash::hash(&endpoints);
        Self { hash, endpoints }
    }

    pub(crate) fn from_dns_addrs(addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
        let mut endpoints = BTreeMap::new();
        let endpoint_addrs = addrs.into_iter().collect();
        endpoints.insert(Locality::Unknown, endpoint_addrs);

        Self::new(endpoints)
    }

    pub(crate) fn len(&self) -> usize {
        self.endpoints.values().map(|v| v.len()).sum()
    }

    /// Returns an iterator over all endpoints in the group.
    ///
    /// Iteration order is guaranteed to be stable as long as the EndpointGroup is
    /// not modified, and guaranteed to consecutively produce all addresses in a single
    /// locality.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &SocketAddr> {
        self.endpoints.values().flatten()
    }

    /// Return the nth address in this group. The order
    pub(crate) fn nth(&self, n: usize) -> Option<&SocketAddr> {
        let mut n = n;
        for endpoints in self.endpoints.values() {
            if n < endpoints.len() {
                return Some(&endpoints[n]);
            }
            n -= endpoints.len();
        }

        None
    }
}

#[cfg(test)]
mod test {
    use std::net::Ipv4Addr;

    use http::StatusCode;
    use junction_api::{Duration, Service};

    use crate::Url;

    use super::*;

    #[test]
    fn test_endpoint_should_retry_no_policy() {
        let mut endpoint = new_endpoint();
        endpoint.retry = None;

        assert!(!endpoint.should_retry(HttpResult::StatusFailed));
        assert!(!endpoint.should_retry(HttpResult::StatusError(
            http::StatusCode::SERVICE_UNAVAILABLE
        )));
    }

    #[test]
    fn test_endpoint_should_retry_with_policy() {
        let mut endpoint = new_endpoint();
        endpoint.retry = Some(RouteRetry {
            codes: vec![StatusCode::BAD_REQUEST.as_u16()],
            attempts: Some(3),
            backoff: Some(Duration::from_secs(2)),
        });

        assert!(endpoint.should_retry(HttpResult::StatusFailed));
        assert!(endpoint.should_retry(HttpResult::StatusError(StatusCode::BAD_REQUEST)));
        assert!(!endpoint.should_retry(HttpResult::StatusError(StatusCode::SERVICE_UNAVAILABLE)));
    }

    #[test]
    fn test_endpoint_should_retry_with_history() {
        let mut endpoint = new_endpoint();
        endpoint.retry = Some(RouteRetry {
            codes: vec![StatusCode::BAD_REQUEST.as_u16()],
            attempts: Some(3),
            backoff: Some(Duration::from_secs(2)),
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
        let backend = Service::dns(url.hostname()).unwrap().as_backend_id(443);
        let address = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 443);

        Endpoint {
            method: http::Method::GET,
            url,
            headers: Default::default(),
            backend,
            address,
            timeouts: None,
            retry: None,
            trace: Trace::new(),
            previous_addrs: vec![],
        }
    }
}
