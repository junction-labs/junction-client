use crate::{
    dns::{self, StdlibResolver},
    trace::Trace,
    xds::{self, AdsClient, ResourceType, XdsCache},
    Endpoint, Error,
};
use futures::{stream::FuturesOrdered, StreamExt};
use rand::{distributions::WeightedError, seq::SliceRandom};
use serde::Deserialize;
use std::{
    borrow::Cow,
    collections::HashMap,
    time::{Duration, Instant},
};
use std::{net::SocketAddr, sync::Arc};

macro_rules! with_deadline {
    ($fut:expr, $deadline:expr, $msg:expr, $trace:expr $(,)*) => {
        tokio::select! {
            biased;

            res = $fut => res,
            _ = sleep_until($deadline) => {
                return Err(Error::timed_out($msg, $trace));
            }
        }
    };
}

/// An outgoing HTTP Request, before any rewrites or modifications have been
/// made.
///
/// Requests are a collection of references and are cheap to clone.
#[derive(Debug, Clone)]
pub struct HttpRequest<'a> {
    /// The HTTP Method of the request.
    method: &'a http::Method,

    /// The request URL, before any rewrites or modifications have been made.
    url: &'a crate::Url,

    /// The request headers, before
    headers: &'a http::HeaderMap,
}

impl<'a> HttpRequest<'a> {
    /// Create a request from individual parts.
    pub fn from_parts(
        method: &'a http::Method,
        url: &'a crate::Url,
        headers: &'a http::HeaderMap,
    ) -> Self {
        Self {
            method,
            url,
            headers,
        }
    }
}

/// The result of selecting an endpoint (see [Client::select_endpoint]).
pub struct SelectedEndpoint {
    /// The selected endpoint address
    pub addr: SocketAddr,

    trace: Trace,
}

/// The result of making an HTTP request.
#[derive(Debug, Clone)]
pub enum HttpResult {
    /// The client received a complete HTTP response with a status code that was
    /// not a client error (4xx) or a server error (5xx).
    StatusOk(http::StatusCode),

    /// The client received a complete HTTP response with a status code that was
    /// a client error (4xx) or a server error (5xx).
    StatusError(http::StatusCode),

    /// The client didn't receive a complete HTTP response. This covers any IO
    /// error or protocol error. From the Junction client's point of view, there
    /// is no point in distinguishing them.
    StatusFailed,
}

impl HttpResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::StatusOk(_))
    }

    pub fn from_u16(code: u16) -> Result<Self, http::status::InvalidStatusCode> {
        let code = http::StatusCode::from_u16(code)?;
        Ok(Self::from_code(code))
    }

    pub fn from_code(code: http::StatusCode) -> Self {
        if code.is_client_error() || code.is_server_error() {
            Self::StatusError(code)
        } else {
            Self::StatusOk(code)
        }
    }
}

/// A service discovery client that looks up URL information based on URLs,
/// headers, and methods.
///
/// Clients use a shared in-memory cache to keep data warm so that a request
/// never has to block on a remote service.
///
/// Clients are cheaply cloneable, and should be cloned to create multiple
/// clients that share the same in-memory cache.
#[derive(Clone)]
pub struct Client {
    // resolve options
    //
    // TODO: make configurable with a builder or something, not sure if they
    // will survive.
    resolve_timeout: Duration,

    // configuration used when searching for additional possible resolved routes currently by
    // expanding the search over the set of possible authority matches.
    search_config: SearchConfig,

    // the ADS client used to fetch xDS config from the control plane
    ads: AdsClient,

    // the DNS resolver used to fetch DNS endpoints when appropriate
    dns: StdlibResolver,
}

#[derive(Clone, Default, Deserialize)]
pub struct SearchConfig {
    // ndots, like dns, but for resolving junction names.
    //
    // like dns, names only use the search path if they contain fewer than
    // `ndots` dots. unlike dns, names are all resolved in-order.
    pub ndots: u8,

    // the list of suffixes searched during hostname lookup. only consulted if the number of
    // dots in a url's hostname is less than `ndots`.
    pub search: Vec<String>,
}

impl SearchConfig {
    pub fn new(ndots: u8, search: Vec<String>) -> Self {
        Self { ndots, search }
    }
}

pub struct ClientBuilder {
    node: String,
    cluster: String,
    resolve_timeout: Duration,
    search_config: Option<SearchConfig>,
    dns_lookup_interval: Duration,
    dns_lookup_jitter: Duration,
    dns_threads: usize,
}

// TODO: methods allow configuring dns resolution
impl ClientBuilder {
    fn new(node: String, cluster: String) -> Self {
        Self {
            node,
            cluster,
            resolve_timeout: Duration::from_secs(5),
            search_config: None,
            dns_lookup_interval: Duration::from_secs(5),
            dns_lookup_jitter: Duration::from_millis(500),
            dns_threads: 2,
        }
    }

    /// Set the resolution timeout.
    pub fn resolve_timeout(mut self, timeout: Duration) -> Self {
        self.resolve_timeout = timeout;
        self
    }

    /// Set the search config. If not set, attempts to parse `/etc/resolv.conf`
    /// and match it's search and ndots settings.
    pub fn search_config(mut self, config: SearchConfig) -> Self {
        self.search_config = Some(config);
        self
    }

    /// Build a new dynamic client, spawning a new ADS client in the background.
    ///
    /// This method creates a new ADS client and ADS connection. xDS data will
    /// not be shared with existing clients. To create a client that shares data
    /// with existing clients, [clone][Client::clone] an existing client.
    ///
    /// This function assumes that you're currently running the context of a
    /// `tokio` runtime and spawns background work on a tokio executor.
    pub async fn build(self, address: String) -> Result<Client, Box<dyn std::error::Error>> {
        let resolve_timeout = self.resolve_timeout;
        let search_config = self.search_config.unwrap_or_else(|| {
            match dns::load_config("/etc/resolv.conf") {
                Ok(config) => SearchConfig::new(config.ndots, config.search),
                // ignore any errors and set this to defaults
                Err(_) => SearchConfig::default(),
            }
        });
        let dns = StdlibResolver::new_with(
            self.dns_lookup_interval,
            self.dns_lookup_jitter,
            self.dns_threads,
        );

        let ads = AdsClient::build(dns.clone(), address, self.node, self.cluster).await?;

        Ok(Client {
            resolve_timeout,
            search_config,
            ads,
            dns,
        })
    }
}

// FIXME: Vec<Endpoints> is probably the wrong thing to return from all our
// resolve methods. We probably need a struct that has something like a list
// of primary endpoints to cycle through on retries, and a separate list of
// endpoints to mirror traffic to. Figure that out once we support mirroring.

impl Client {
    /// Create a new client builder.
    pub fn builder(node: String, cluster: String) -> ClientBuilder {
        ClientBuilder::new(node, cluster)
    }

    /// Resolve an HTTP method, URL, and headers into an [Endpoint].
    ///
    /// This is the main entry point into Junction. When building an
    /// integration, use this method to fetch an initial endpoint. After making
    /// an initial request, use [report_status][Self::report_status] to report
    /// the status of the request and to retry on failure.
    ///
    /// The endpoint returned from this method should be a complete description
    /// of how to make an HTTP request - it contains the IP address to use, the
    /// full URL and hostname, the complete set of headers, and retry and timeout
    /// policy the client should use to make a request.
    pub async fn resolve_http(
        &self,
        method: &http::Method,
        url: &crate::Url,
        headers: &http::HeaderMap,
    ) -> crate::Result<Endpoint> {
        let deadline = Instant::now() + self.resolve_timeout;

        let request = HttpRequest::from_parts(method, url, headers);

        let resolved = resolve_route(
            &self.ads,
            &self.search_config,
            Trace::new(),
            request.clone(),
            Some(deadline),
        )
        .await?;

        // select endpoints using the result of route resolution
        let selected = select_endpoint(
            &self.ads,
            &self.dns,
            resolved.trace,
            RequestContext {
                request_hash: resolved.request_hash,
                previous_addrs: &[],
            },
            &resolved.cluster,
            Some(deadline),
        )
        .await?;

        let address = selected.addr;
        let trace = selected.trace;

        Ok(Endpoint {
            method: method.clone(),
            url: url.clone(),
            headers: headers.clone(),
            request_hash: resolved.request_hash,
            cluster_name: resolved.cluster,
            address,
            previous_addrs: vec![],
            retries: resolved.retries.map(|r| r.into()),
            timeouts: resolved.timeouts.map(|t| t.into()),
            trace,
        })
    }

    /// Report the status of an externally made HTTP request made against an
    /// [Endpoint] returned from `resolve_http`.
    ///
    /// If retrying the response is appropriate, a new Endpoint will be returned
    /// with updated address and host info set - calling `resolve_http` to start
    /// a retry attempt will drop request history and may result in too many
    /// retries.
    ///
    /// If a retry is not appropriate, the returned Endpoint will have updated
    /// history information, but request details will remain the same. Clients
    /// may use that value for status or error reporting.
    pub async fn report_status(
        &self,
        endpoint: Endpoint,
        response: HttpResult,
    ) -> crate::Result<Endpoint> {
        // TODO: track response stats per address

        // if there's no reason to pick a new endpoint, just return the existing one as-is
        if response.is_ok() || !endpoint.should_retry(response) {
            return Ok(endpoint);
        }

        // redo endpoint selection. this should use the same cluster that was
        // used in the initial request, and the same request hash, but should
        // not necessarily pick the same endpoint.
        let deadline = Instant::now() + self.resolve_timeout;
        let next = select_endpoint(
            &self.ads,
            &self.dns,
            endpoint.trace,
            RequestContext {
                request_hash: endpoint.request_hash,
                previous_addrs: &endpoint.previous_addrs,
            },
            &endpoint.cluster_name,
            Some(deadline),
        )
        .await?;

        // track address history
        let mut previous_addrs = endpoint.previous_addrs;
        previous_addrs.push(endpoint.address);

        Ok(Endpoint {
            address: next.addr,
            trace: next.trace,
            previous_addrs,
            ..endpoint
        })
    }

    /// Start a gRPC CSDS server on the given port. To run the server, you must
    /// `await` this future.
    ///
    /// For static clients, this does nothing.
    pub async fn csds_server(self, port: u16) -> Result<(), tonic::transport::Error> {
        self.ads.csds_server(port).await
    }

    /// Dump the client's current cache of xDS resources, as fetched from the
    /// config server.
    ///
    /// This is a programmatic view of the same data that you can fetch over
    /// gRPC by starting a [Client::csds_server].
    pub fn dump_xds(&self) -> impl Iterator<Item = crate::XdsConfig> + '_ {
        self.ads.iter_xds()
    }

    /// Dump xDS resources that failed to update. This is a view of the data
    /// returned by [Client::dump_xds] that only contains resources with
    /// errors.
    pub fn dump_xds_errors(&self) -> impl Iterator<Item = crate::XdsConfig> + '_ {
        self.ads.iter_xds().filter(|x| x.last_error.is_some())
    }
}

// this is basically Either<_, > so that we don't have to deal with borrowck
// while resolving_routes
enum RouteConfigRef {
    Route(Arc<xds::RouteConfiguration>),
    Inlined(Arc<xds::ApiListener>),
}

impl AsRef<xds::RouteConfiguration> for RouteConfigRef {
    fn as_ref(&self) -> &xds::RouteConfiguration {
        match self {
            RouteConfigRef::Route(route) => route,
            RouteConfigRef::Inlined(listener) => match &listener.route_config {
                xds::listeners::RouteConfig::Inline(route) => route,
                _ => panic!("expected an inline RouteConfiguration"),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedRoute {
    pub(crate) cluster: xds::ResourceName,
    pub(crate) request_hash: u64,
    pub(crate) retries: Option<xds::route_configs::Retries>,
    pub(crate) timeouts: Option<xds::route_configs::Timeouts>,
    pub(crate) trace: Trace,
}

pub(crate) async fn resolve_route(
    cache: &impl XdsCache,
    search_config: &SearchConfig,
    mut trace: Trace,
    request: HttpRequest<'_>,
    deadline: Option<Instant>,
) -> crate::Result<ResolvedRoute> {
    let uris_to_search = search(search_config, request.url);
    assert!(
        !uris_to_search.is_empty(),
        "URI search is empty, this is a bug in Junction."
    );

    // fetch the first listener that resolves, returning the first error we see.
    //
    // using FuturesOrdered means that the responses will come back in
    // search_path order - a valid not-found response is not necessarily a
    // signal and we should fall back to the next response in the path, but a
    // timeout or error means we should stop processing immediately and return
    // the error.
    let mut futures_ordered = FuturesOrdered::new();
    for url in uris_to_search {
        let name = xds::ResourceName::from(url.authority().to_string());
        futures_ordered.push_back(async move {
            cache
                .subscribe(xds::ResourceType::Listener, name.clone())
                .await;
            cache.get_listener(&name).await.map(|l| (name, url, l))
        });
    }

    let (listener_name, url, listener) = loop {
        match with_deadline!(futures_ordered.next(), deadline, "fetching listener", trace) {
            Some(Some(found)) => break found,
            Some(None) => {
                continue;
            }
            None => {
                return Err(Error::no_route_matched(
                    request.url.authority().to_string(),
                    trace,
                ));
            }
        }
    };
    trace.lookup_listener(listener_name.to_string());

    // rewrite the request so that we're now using the URL from the search path
    // that matched instead of the raw URL.
    let request = HttpRequest {
        url: url.as_ref(),
        headers: request.headers,
        method: request.method,
    };

    // immdediately try to fetch the route with the listener name.
    let route_config = match &listener.route_config {
        xds::listeners::RouteConfig::Rds(name) => {
            match with_deadline!(
                cache.get_route_config(name),
                deadline,
                "fetching route",
                trace
            ) {
                Some(route) => {
                    trace.lookup_route(name.to_string());
                    RouteConfigRef::Route(route)
                }
                None => {
                    return Err(Error::not_found(
                        ResourceType::RouteConfiguration.type_url().to_string(),
                        name.to_string(),
                        trace,
                    ))
                }
            }
        }
        xds::listeners::RouteConfig::Inline(_) => RouteConfigRef::Inlined(listener),
    };

    // match the request against the list of RouteRules that are part of this
    // request. the hostname and port of the request have already matched but we
    // need to match headers/url params/method and so on.
    let action = match find_match(route_config.as_ref(), request.clone()) {
        Some(action) => action,
        None => {
            return Err(Error::no_route_matched(
                request.url.authority().to_string(),
                trace,
            ))
        }
    };
    trace.matched_route();

    // pick a target at random from the list, respecting weights. if there are
    // no backends listed we should blackhole here.
    let cluster = match &action.cluster {
        xds::route_configs::ClusterSpecifier::Cluster(name) => name,
        xds::route_configs::ClusterSpecifier::Weighted(clusters) => {
            match crate::rand::with_thread_rng(|rng| clusters.choose_weighted(rng, |c| c.weight)) {
                Ok(cluster) => &cluster.name,
                Err(WeightedError::NoItem) => {
                    return Err(Error::unavailable(trace, "route has no backends"));
                }
                Err(_) => return Err(Error::unavailable(trace, "route has invalid weights")),
            }
        }
    };
    trace.select_cluster();

    // if there's nothign in the request that matches this hash policy, fall
    // back to essentially random with sticky sessions. this is what envoy
    // does with the rationale that this is better than failing the request
    // if the config is bad.
    //
    // https://github.com/envoyproxy/envoy/blob/73fe00fc139fd5053f4c4a5d66569cc254449896/source/extensions/load_balancing_policies/common/thread_aware_lb_impl.cc#L157-L164
    let request_hash =
        hash_request(request.clone(), &action.hash_policies).unwrap_or_else(crate::rand::random);
    trace.hash_request(request_hash);

    Ok(ResolvedRoute {
        trace,
        cluster: cluster.clone(),
        request_hash,
        retries: action.retries.clone(),
        timeouts: action.timeouts.clone(),
    })
}

#[derive(Debug, Clone)]
struct RequestContext<'a> {
    request_hash: u64,
    previous_addrs: &'a [SocketAddr],
}

async fn select_endpoint(
    cache: &impl XdsCache,
    dns: &StdlibResolver,
    mut trace: Trace,
    request: RequestContext<'_>,
    cluster_name: &xds::ResourceName,
    deadline: Option<Instant>,
) -> crate::Result<SelectedEndpoint> {
    trace.start_endpoint_selection();

    // lookup cluster
    let cluster = with_deadline!(
        cache.get_cluster(cluster_name),
        deadline,
        "fetching cluster",
        trace,
    );
    let Some(cluster) = cluster else {
        return Err(Error::not_found(
            ResourceType::Cluster.type_url().to_string(),
            cluster_name.to_string(),
            trace,
        ));
    };
    trace.lookup_cluster(cluster_name.to_string());

    // lookup endpoints for a cluster
    let load_assignment = match &cluster.endpoints {
        xds::clusters::Endpoints::Eds(name) => {
            let load_assignment = with_deadline!(
                cache.get_load_assignment(name),
                deadline,
                "fetching endpoints",
                trace
            );
            match load_assignment {
                Some(load_assignment) => {
                    trace.lookup_endpoints(name.to_string());
                    load_assignment
                }
                None => {
                    return Err(Error::not_found(
                        ResourceType::ClusterLoadAssignment.type_url().to_string(),
                        name.to_string(),
                        trace,
                    ));
                }
            }
        }
        xds::clusters::Endpoints::LogicalDns { hostname, port } => {
            let load_assignment = with_deadline!(
                dns.get_endpoints_await(hostname, *port),
                deadline,
                "dns resolution",
                trace
            );
            match load_assignment {
                Some(load_assignment) => {
                    trace.lookup_dns(hostname.clone());
                    load_assignment
                }
                None => {
                    return Err(Error::not_found(
                        "dns".to_string(),
                        hostname.to_string(),
                        trace,
                    ));
                }
            }
        }
    };
    let Some(endpoints) = load_assignment.endpoints.first() else {
        return Err(Error::unavailable(trace, "no available endpoint groups"));
    };

    // load balance.
    //
    // no trace is done here, the load balancer impls stamp the traces themselves
    let addr = cluster.load_balancer.load_balance(
        &mut trace,
        request.request_hash,
        endpoints,
        request.previous_addrs,
    );
    let Some(addr) = addr else {
        return Err(Error::unavailable(trace, "no available end points"));
    };

    Ok(SelectedEndpoint { addr: *addr, trace })
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

fn find_match<'a>(
    route: &'a xds::RouteConfiguration,
    request: HttpRequest<'_>,
) -> Option<&'a xds::route_configs::Action> {
    // FIXME: support match order. we have to follow xDS search order for
    // domains instead of just picking the first match. we don't want to support
    // just the wildcard but the others need to be handled appropriately.
    //
    // - Exact domain names: www.foo.com.
    // - Suffix domain wildcards: *.foo.com or *-bar.foo.com.
    // - Prefix domain wildcards: foo.* or foo-*.
    // - Special wildcard * matching any domain.
    //
    // https://www.envoyproxy.io/docs/envoy/latest/api-v3/config/route/v3/route_components.proto#envoy-v3-api-msg-config-route-v3-routematch
    let mut matching_vhost = None;
    let hostname = request.url.hostname();
    for vhost in &route.vhosts {
        if vhost.domains.iter().any(|d| d.matches_hostname(hostname)) {
            matching_vhost = Some(vhost);
        }
    }

    let route = matching_vhost?
        .routes
        .iter()
        .find(|route| matches_request(&route.matcher, &request))?;

    Some(&route.action)
}

fn matches_request(matcher: &xds::route_configs::Matcher, request: &HttpRequest<'_>) -> bool {
    is_method_match(&matcher.method, request.method)
        && is_path_match(&matcher.path, request.url)
        && is_header_match(&matcher.headers, request.headers)
        && is_query_match(&matcher.query, request.url)
}

#[inline]
fn is_method_match(
    method_match: &Option<xds::route_configs::MethodMatcher>,
    method: &http::Method,
) -> bool {
    match method_match.as_ref() {
        None => true,
        Some(m) => m.is_match(method),
    }
}

#[inline]
fn is_path_match(path_match: &xds::route_configs::PathMatcher, url: &crate::Url) -> bool {
    path_match.matches_path(url.path())
}

#[inline]
fn is_header_match(
    header_matches: &[xds::route_configs::HeaderMatcher],
    headers: &http::HeaderMap,
) -> bool {
    header_matches.iter().all(|h| {
        let header_val = headers.get(&h.name).map(|val| val.as_bytes());
        h.matches_value(header_val)
    })
}

fn is_query_match(query_matches: &[xds::route_configs::QueryMatcher], url: &crate::Url) -> bool {
    let Some(query) = url.query() else {
        return query_matches.is_empty();
    };

    let query: HashMap<_, _> = form_urlencoded::parse(query.as_bytes()).collect();

    query_matches.iter().all(|q| {
        let query_val = query.get(&Cow::Borrowed(q.name.as_str()));
        q.matches(query_val.as_ref().map(|cow| cow.as_ref()))
    })
}

/// Hash an outgoing request based on a set of hash policies.
///
/// Like Envoy and gRPC, multiple hash policies are combined by applying a
/// bitwise left-rotate to the previous value and xor-ing the new value into
/// the previous value.
///
/// See:
/// - https://github.com/grpc/proposal/blob/master/A42-xds-ring-hash-lb-policy.md#xds-api-fields
/// - https://github.com/envoyproxy/envoy/blob/73fe00fc139fd5053f4c4a5d66569cc254449896/source/common/http/hash_policy.cc#L251-L272
fn hash_request(
    request: HttpRequest<'_>,
    hash_policies: &[xds::route_configs::HashPolicy],
) -> Option<u64> {
    let mut hash: Option<u64> = None;

    for hash_policy in hash_policies {
        if let Some(new_hash) = hash_component(hash_policy, request.url, request.headers) {
            hash = Some(match hash {
                Some(hash) => hash.rotate_left(1) ^ new_hash,
                None => new_hash,
            });

            if hash_policy.terminal {
                break;
            }
        }
    }

    hash
}

fn hash_component(
    policy: &xds::route_configs::HashPolicy,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Option<u64> {
    use crate::hash::thread_local_xxhash;
    use xds::route_configs::RequestHasher;

    match &policy.hasher {
        RequestHasher::Header { name } => {
            let mut header_values: Vec<_> = headers
                .get_all(name)
                .iter()
                .map(http::HeaderValue::as_bytes)
                .collect();

            if header_values.is_empty() {
                None
            } else {
                // sort values so that "foo,bar" and "bar,foo" hash to the same value
                header_values.sort();
                Some(thread_local_xxhash::hash_iter(header_values))
            }
        }
        RequestHasher::QueryParameter { ref name } => url.query().map(|query| {
            let matching_vals = form_urlencoded::parse(query.as_bytes())
                .filter_map(|(param, value)| (&param == name).then_some(value));
            thread_local_xxhash::hash_iter(matching_vals)
        }),
    }
}

/// generate a URL search path for this url.
///
/// the resturned Vec will always contain either:
///
/// - a single element, a ref to the original URL
///
/// - `search.len() + 1` elements, where the first element is the original
///   URl and the rest of the entries are the result of appending the URL's
///   hostname to the suffixes in search_config.search. the order of the suffixes in
///   search is preserved.
fn search<'a>(search_config: &SearchConfig, url: &'a crate::Url) -> Vec<Cow<'a, crate::Url>> {
    // TODO: this could return an enum { Original(url), Search(url, path) } that
    // implements Iterator and lazily generates Cow<Url>. there's no reason to
    // do that at the moment but it'd be a little more correct.

    let hostname = url.hostname();
    let dots = hostname.as_bytes().iter().filter(|&&b| b == b'.').count();

    let mut urls = vec![Cow::Borrowed(url)];

    if dots < search_config.ndots as usize {
        for suffix in &search_config.search {
            let mut new_hostname = String::with_capacity(hostname.len() + hostname.len() + 1);
            new_hostname.push_str(hostname);
            new_hostname.push('.');
            new_hostname.push_str(suffix);

            let new_url = url
                .with_hostname(&new_hostname)
                .expect("SearchConfig search produced an invalid URL. this is a bug in Junction");
            urls.push(Cow::Owned(new_url));
        }
    }

    urls
}

// TODO: thorough tests for matching

#[cfg(test)]
mod test {
    use crate::{xds::ResourceName, Url};
    use std::str::FromStr;

    use futures::FutureExt;
    use pretty_assertions::assert_eq;

    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn assert_send_sync() {
        assert_send::<HttpRequest<'_>>();
        assert_sync::<HttpRequest<'_>>();
    }

    #[test]
    fn test_search() {
        let url = Url::from_str("https://tasty.potato.tomato:9876").unwrap();
        let search_setup: Vec<_> = ["foo.bar.baz", "bar.baz", "baz"]
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        // with ndots < dots, should just return the original url
        assert_eq!(
            search(&SearchConfig::new(0, search_setup.clone()), &url),
            vec![Cow::Borrowed(&url)]
        );
        assert_eq!(
            search(&SearchConfig::new(1, search_setup.clone()), &url),
            vec![Cow::Borrowed(&url)]
        );
        assert_eq!(
            search(&SearchConfig::new(2, search_setup.clone()), &url),
            vec![Cow::Borrowed(&url)]
        );

        // with high-enough ndots should return a borrowed URL and owned URLs
        assert_eq!(
            search(&SearchConfig::new(3, search_setup), &url),
            vec![
                Cow::Borrowed(&url),
                Cow::Owned(
                    "https://tasty.potato.tomato.foo.bar.baz:9876"
                        .parse()
                        .unwrap()
                ),
                Cow::Owned("https://tasty.potato.tomato.bar.baz:9876".parse().unwrap()),
                Cow::Owned("https://tasty.potato.tomato.baz:9876".parse().unwrap()),
            ],
        );
    }

    #[track_caller]
    fn assert_resolve_routes(cache: &impl XdsCache, request: HttpRequest<'_>) -> ResolvedRoute {
        resolve_route(cache, &SearchConfig::default(), Trace::new(), request, None)
            .now_or_never()
            .unwrap()
            .unwrap()
    }

    #[test]
    fn resolve_route_any_match() {
        let cache = xds::StaticCache::with_xds(&[
            xds::test::listener!("example.com:80", "passthrough-route"),
            xds::test::listener!("example.com:443", "passthrough-route"),
            xds::test::listener!("example.com:8008", "passthrough-route"),
            xds::test::route_config!(
                "passthrough-route",
                vec![xds::test::vhost!(
                    "test-vhost",
                    ["example.com"],
                    [xds::test::route!(default "cluster.example:8008")]
                )]
            ),
        ])
        .unwrap();

        // check with no port
        let url = Url::from_str("http://example.com/test-path").unwrap();
        let headers = http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers);

        let resolved = assert_resolve_routes(&cache, request);
        assert_eq!(resolved.cluster, ResourceName::from("cluster.example:8008"));

        // check with explicit ports
        for port in [80, 443, 8008] {
            let url = Url::from_str(&format!("http://example.com:{port}/test-path")).unwrap();
            let headers = http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers);

            let resolved = assert_resolve_routes(&cache, request);
            assert_eq!(resolved.cluster, ResourceName::from("cluster.example:8008"));
        }
    }

    #[test]
    fn resolve_route_any_match_search() {
        let cache = xds::StaticCache::with_xds(&[
            xds::test::listener!("example.default.svc.cluster.local:443", "test-route"),
            xds::test::route_config!(
                "test-route",
                vec![xds::test::vhost!(
                    "test-vhost",
                    ["example.default.svc.cluster.local"],
                    [xds::test::route!(default "example.default.svc.cluster.local:8008")]
                )]
            ),
        ])
        .unwrap();

        let search_config = SearchConfig {
            ndots: 5,
            search: [
                "default.svc.cluster.local",
                "svc.cluster.local",
                "cluster.local",
            ]
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        };

        for hostname in ["example", "example.default"] {
            let url = Url::from_str(&format!("https://{hostname}/test-path")).unwrap();
            let headers = http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers);

            let resolved = resolve_route(&cache, &search_config, Trace::new(), request, None)
                .now_or_never()
                .unwrap()
                .unwrap();
            assert_eq!(
                resolved.cluster,
                ResourceName::from("example.default.svc.cluster.local:8008")
            );
        }
    }

    #[test]
    fn resolve_path_match() {
        let cache = xds::StaticCache::with_xds(&[
            xds::test::listener!("example.com:80", "passthrough-route"),
            crate::xds::test::route_config!(
                "passthrough-route",
                "v123",
                (vec![xds::test::vhost!(
                    "test-vhost",
                    ["example.com"],
                    [
                        xds::test::route!(exact_path "/v1/users" => "cluster2.example:8008"),
                        xds::test::route!(default "cluster.example:8008"),
                    ]
                )])
            ),
        ])
        .unwrap();

        // check with no port
        let url = Url::from_str("http://example.com/test-path").unwrap();
        let headers = http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers);

        let resolved = assert_resolve_routes(&cache, request);
        assert_eq!(resolved.cluster, ResourceName::from("cluster.example:8008"));

        let url = Url::from_str("http://example.com/v1/users").unwrap();
        let headers = http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers);

        let resolved = assert_resolve_routes(&cache, request);
        assert_eq!(
            resolved.cluster,
            ResourceName::from("cluster2.example:8008")
        );
    }

    #[test]
    fn test_resolve_query_match() {
        let cache = xds::StaticCache::with_xds(&[
            xds::test::listener!("example.com:80", "passthrough-route"),
            crate::xds::test::route_config!(
                "passthrough-route",
                "v123",
                (vec![xds::test::vhost!(
                    "test-vhost",
                    ["example.com"],
                    [
                        xds::test::route!(query "qp1", "potato" => "new-cluster.example:8008"),
                        xds::test::route!(default "default-cluster.example:8008"),
                    ]
                )])
            ),
        ])
        .unwrap();

        cache.get_route_config(&ResourceName::from("passthrough-route"));

        let wont_match = [
            "http://example.com?qp1=tomato",
            "http://example.com?qp1=potatooo",
            "http://example.com?qp2=barfoo",
            "http://example.com?qp2=fobar",
            "http://example.com?qp1=potat&qp2=foobar",
        ];

        for url in wont_match {
            let url = Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, headers);

            let resolved = assert_resolve_routes(&cache, request);
            // should match the fallthrough rule
            assert_eq!(
                resolved.cluster,
                ResourceName::from("default-cluster.example:8008"),
                "{url}",
            );
        }

        let will_match = [
            "http://example.com?qp1=potato&qp2=foobar",
            "http://example.com?qp1=potato&qp2=foobazbar",
            "http://example.com?qp1=potato&qp2=fooooooooooooooobar",
        ];

        for url in will_match {
            let url = Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, headers);

            let resolved = assert_resolve_routes(&cache, request);
            assert_eq!(
                resolved.cluster,
                ResourceName::from("new-cluster.example:8008")
            );
        }
    }
}
