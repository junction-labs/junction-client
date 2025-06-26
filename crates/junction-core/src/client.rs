use crate::{
    dns,
    xds::{self, AdsClient, ResourceType},
    Endpoint, Error, Trace,
};
use futures::{stream::FuturesOrdered, StreamExt};
use junction_api::Hostname;
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

    config: Arc<DynamicConfig>,
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
    pub search: Vec<Hostname>,
}

impl SearchConfig {
    pub fn new(ndots: u8, search: Vec<Hostname>) -> Self {
        Self { ndots, search }
    }
}

struct DynamicConfig {
    ads: AdsClient,

    /// a the shared handle to the task that's actually running the client in
    /// the background. should not drop until every active client drops.
    ///
    /// TODO: should this get bundled into AdsClient? shrug emoji?
    #[allow(unused)]
    task_handle: tokio::task::JoinHandle<()>,
}

// FIXME: Vec<Endpoints> is probably the wrong thing to return from all our
// resolve methods. We probably need a struct that has something like a list
// of primary endpoints to cycle through on retries, and a separate list of
// endpoints to mirror traffic to. Figure that out once we support mirroring.

impl Client {
    /// Build a new dynamic client, spawning a new ADS client in the background.
    ///
    ///This method creates a new ADS client and ADS connection. Dynamic data
    ///will not be shared with existing clients. To create a client that shares
    ///data with existing clients, [clone][Client::clone] an existing client.
    ///
    /// This function assumes that you're currently running the context of a
    /// `tokio` runtime and spawns background work on a tokio executor.
    pub async fn build(
        address: String,
        node_id: String,
        cluster: String,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (ads, mut ads_task) = AdsClient::build(address, node_id, cluster).unwrap();

        // try to start the ADS connection while blocking. if it fails, fail
        // fast here instead of letting the client start.
        //
        // once it's started, hand off the task to the executor in the
        // background.
        ads_task.connect().await?;
        let handle = tokio::spawn(async move {
            match ads_task.run().await {
                Ok(()) => (),
                Err(e) => panic!(
                    "junction-core: ads client exited with an unexpected error: {e}. this is a bug in Junction!"
                ),
            }
        });

        // load search-path config from the system.
        //
        // this should eventually be configurable, but for now we're trying
        // resolv.conf to match kube's default behavior out of the box. on other
        // systems this may not be useful yet - that's ok.
        let search_config = match dns::load_config("/etc/resolv.conf") {
            Ok(config) => SearchConfig::new(config.ndots, config.search),
            // ignore any errors and set this to defaults
            Err(_) => SearchConfig::default(),
        };

        // wrap it all up in a dynamic config and return
        let config = Arc::new(DynamicConfig {
            ads,
            task_handle: handle,
        });
        let client = Self {
            resolve_timeout: Duration::from_secs(5),
            search_config,
            config,
        };

        Ok(client)
    }

    /// Build a client with static configuration. This client will use the
    /// passed configuration to resolve routes and backends, but will still
    /// fetch endpoints dynamically.
    ///
    /// This method will panic if the client being cloned is fully static. To
    /// convert a static client to a client that uses dynamic config, create a
    /// new client.
    // pub fn with_static_config(self, routes: Vec<Route>, backends: Vec<Backend>) -> Client {
    //     let static_config = Arc::new(StaticConfig::with_inferred(routes, backends));

    //     let dyn_config = match &self.config {
    //         Config::Static(_) => panic!("can't use dynamic endpoints with a fully static client"),
    //         Config::DynamicEndpoints(_, d) => Arc::clone(d),
    //         Config::Dynamic(d) => Arc::clone(d),
    //     };

    //     let config = Config::DynamicEndpoints(static_config, dyn_config);
    //     Client { config, ..self }
    // }

    /// Construct a client that uses fully static configuration and does not
    /// connect to a control plane at all.
    ///
    /// This is intended to be used to test configuration in controlled settings
    /// or to use Junction an offline mode. Once a client has been converted to
    /// fully static, it's not possible to convert it back to using dynamic
    /// discovery data.
    // pub fn with_static_endpoints(self, routes: Vec<Route>, backends: Vec<Backend>) -> Client {
    //     let static_config = Arc::new(StaticConfig::with_inferred(routes, backends));
    //     let config = Config::Static(static_config);
    //     Client { config, ..self }
    // }

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
            &self.config.ads,
            &self.search_config,
            Trace::new(),
            request.clone(),
            Some(deadline),
        )
        .await?;

        // select endpoints using the result of route resolution
        let selected = select_endpoint(
            &self.config.ads,
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
            &self.config.ads,
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

    /// Resolve an HTTP method, URL, and headers to a target backend, returning
    /// the Route that matched, the index of the rule that matched, and the
    /// backend that was chosen - to make backend choice determinstic with
    /// multiple backends, set the `JUNCTION_SEED` environment variable.
    ///
    /// This is a lower-level method that only performs the Route matching part
    /// of resolution. It's intended for debugging or querying a client for
    /// specific information. For everyday use, prefer [Client::resolve_http].
    // pub async fn resolve_route(
    //     &self,
    //     request: HttpRequest<'_>,
    //     deadline: Option<Instant>,
    // ) -> crate::Result<xds::ResourceName> {
    //     let trace = Trace::new();
    //     resolve_route(&self.config, trace, request, deadline, &self.search_config).await
    // }

    /// Select an endpoint address for this backend from the set of currently
    /// available endpoints.
    ///
    /// This is a lower level method that only performs part of route
    /// resolution, and is intended for debugging and testing. For everyday use,
    /// prefer [Client::resolve_http].
    // pub async fn select_endpoint(
    //     &self,
    //     backend: &BackendId,
    //     ctx: LbContext<'_>,
    //     deadline: Option<Instant>,
    // ) -> crate::Result<SelectedEndpoint> {
    //     select_endpoint(&self.config, backend, ctx, deadline).await
    // }

    /// Start a gRPC CSDS server on the given port. To run the server, you must
    /// `await` this future.
    ///
    /// For static clients, this does nothing.
    pub async fn csds_server(self, port: u16) -> Result<(), tonic::transport::Error> {
        self.config.ads.csds_server(port).await
    }

    /// Dump the client's current cache of xDS resources, as fetched from the
    /// config server.
    ///
    /// This is a programmatic view of the same data that you can fetch over
    /// gRPC by starting a [Client::csds_server].
    pub fn dump_xds(&self) -> impl Iterator<Item = crate::XdsConfig> + '_ {
        self.config.ads.iter_xds()
    }

    /// Dump xDS resources that failed to update. This is a view of the data
    /// returned by [Client::dump_xds] that only contains resources with
    /// errors.
    pub fn dump_xds_errors(&self) -> impl Iterator<Item = crate::XdsConfig> + '_ {
        self.config
            .ads
            .iter_xds()
            .filter(|x| x.last_error.is_some())
    }
}

pub(crate) trait XdsCache {
    async fn subscribe(&self, rtype: xds::ResourceType, name: xds::ResourceName);
    async fn get_listener(&self, name: &xds::ResourceName) -> Option<Arc<xds::ApiListener>>;
    async fn get_route_config(
        &self,
        name: &xds::ResourceName,
    ) -> Option<Arc<xds::RouteConfiguration>>;
    async fn get_cluster(&self, target: &xds::ResourceName) -> Option<Arc<xds::Cluster>>;
    async fn get_load_assignment(
        &self,
        backend: &xds::ResourceName,
    ) -> Option<Arc<xds::LoadAssignment>>;
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
            RouteConfigRef::Route(route) => &route,
            RouteConfigRef::Inlined(listener) => match &listener.route_config {
                xds::listeners::RouteConfig::Inline(route) => &route,
                _ => panic!("expected an inline RouteConfiguration"),
            },
        }
    }
}

pub(crate) struct ResolvedRoute {
    cluster: xds::ResourceName,
    request_hash: u64,
    retries: Option<xds::route_configs::Retries>,
    timeouts: Option<xds::route_configs::Timeouts>,
    trace: Trace,
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
            cache.get_listener(&name).await.map(|l| (name, l))
        });
    }

    let (listener_name, listener) = loop {
        match with_deadline!(futures_ordered.next(), deadline, "fetching listener", trace) {
            Some(Some(found)) => break found,
            Some(None) => {
                continue;
            }
            None => {
                return Err(Error::no_route_matched(
                    request.url.authority().to_string(),
                    trace,
                ))
            }
        }
    };
    trace.lookup_listener(listener_name.to_string());

    // immdediately try to fetch the route
    let route_config = match &listener.route_config {
        xds::listeners::RouteConfig::Rds(name) => {
            match with_deadline!(
                cache.get_route_config(&name),
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
    mut trace: Trace,
    request: RequestContext<'_>,
    cluster_name: &xds::ResourceName,
    deadline: Option<Instant>,
) -> crate::Result<SelectedEndpoint> {
    trace.start_endpoint_selection();

    // lookup cluster
    let cluster = with_deadline!(
        cache.get_cluster(&cluster_name),
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
                cache.get_load_assignment(&name),
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
            // get a handle to the DNS resolver here and call into it
            todo!("support LOGICAL_DNS clusters")
        }
    };
    let Some(endpoints) = load_assignment.endpoints.first() else {
        return Err(Error::unavailable(trace, "no available endpoints"));
    };

    // load balance.
    //
    // no trace is done here, the load balancer impls stamp the traces themselves
    let addr = cluster.load_balancer.load_balance(
        &mut trace,
        request.request_hash,
        &endpoints,
        request.previous_addrs,
    );
    let Some(addr) = addr else {
        return Err(Error::unavailable(trace, "no available endpoints"));
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
    let authority = request.url.authority();
    for vhost in &route.vhosts {
        if vhost.domains.iter().any(|d| d.matches_hostname(&authority)) {
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
    method_match.as_ref().is_some_and(|m| m.is_match(method))
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
    use crate::Url;
    use junction_api::Hostname;
    use std::str::FromStr;

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
        let search_setup: Vec<Hostname> = vec![
            Hostname::from_static("foo.bar.baz"),
            Hostname::from_static("bar.baz"),
            Hostname::from_static("baz"),
        ];

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

    // #[track_caller]
    // fn assert_resolve_routes(cache: &impl ConfigCache, request: HttpRequest<'_>) -> ResolvedRoute {
    //     resolve_routes(cache, Trace::new(), request, None, &SearchConfig::default())
    //         .now_or_never()
    //         .unwrap()
    //         .unwrap()
    // }

    // #[track_caller]
    // fn assert_resolve_err(cache: &impl ConfigCache, request: HttpRequest<'_>) -> crate::Error {
    //     resolve_routes(cache, Trace::new(), request, None, &SearchConfig::default())
    //         .now_or_never()
    //         .unwrap()
    //         .unwrap_err()
    // }

    // #[test]
    // fn test_resolve_passthrough_route() {
    //     let svc = Service::dns("example.com").unwrap();

    //     let routes = StaticConfig::new(
    //         vec![Route::passthrough_route(
    //             Name::from_static("example"),
    //             svc.clone(),
    //         )],
    //         vec![],
    //     );

    //     // check with no port
    //     let url = Url::from_str("http://example.com/test-path").unwrap();
    //     let headers = http::HeaderMap::default();
    //     let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

    //     let resolved = assert_resolve_routes(&routes, request);
    //     assert_eq!(resolved.backend, svc.as_backend_id(80));

    //     // check with explicit ports
    //     for port in [443, 8008] {
    //         let url = Url::from_str(&format!("http://example.com:{port}/test-path")).unwrap();
    //         let headers = http::HeaderMap::default();
    //         let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

    //         let resolved = assert_resolve_routes(&routes, request);
    //         assert_eq!(resolved.backend, svc.as_backend_id(port));
    //     }
    // }

    // #[test]
    // fn test_resolve_route_no_rules() {
    //     let route = Route {
    //         id: Name::from_static("no-rules"),
    //         hostnames: vec![Hostname::from_static("example.com").into()],
    //         ports: vec![],
    //         tags: Default::default(),
    //         rules: vec![],
    //     };

    //     let routes = StaticConfig::new(vec![route], vec![]);

    //     let url = Url::from_str("http://example.com:3214/users/123").unwrap();
    //     let headers = http::HeaderMap::default();
    //     let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

    //     let err = assert_resolve_err(&routes, request);
    //     assert!(err.to_string().contains("no rules matched the request"));
    //     assert!(!err.is_temporary());
    // }

    // #[test]
    // fn test_resolve_route_no_rules_with_search_config() {
    //     let route = Route {
    //         id: Name::from_static("no-rules"),
    //         hostnames: vec![Hostname::from_static("example.com").into()],
    //         ports: vec![],
    //         tags: Default::default(),
    //         rules: vec![],
    //     };

    //     let routes = StaticConfig::new(vec![route], vec![]);

    //     let url = Url::from_str("http://example.com:3214/users/123").unwrap();
    //     let headers = http::HeaderMap::default();
    //     let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

    //     let err = resolve_routes(
    //         &routes,
    //         Trace::new(),
    //         request,
    //         None,
    //         &SearchConfig::new(2, vec![Hostname::from_static("example.com")]),
    //     )
    //     .now_or_never()
    //     .unwrap()
    //     .unwrap_err();

    //     assert!(err.to_string().contains("no rules matched the request"));
    //     assert!(!err.is_temporary());
    // }

    // #[test]
    // fn test_resolve_route_no_backends() {
    //     let route = Route {
    //         id: Name::from_static("no-backends"),
    //         hostnames: vec![Hostname::from_static("example.com").into()],
    //         ports: vec![],
    //         tags: Default::default(),
    //         rules: vec![RouteRule {
    //             matches: vec![RouteMatch {
    //                 path: Some(PathMatch::Prefix {
    //                     value: "".to_string(),
    //                 }),
    //                 ..Default::default()
    //             }],
    //             ..Default::default()
    //         }],
    //     };

    //     let routes = StaticConfig::new(vec![route], vec![]);

    //     for port in [80, 7887] {
    //         let method = &http::Method::GET;
    //         let url = &Url::from_str(&format!("http://example.com:{port}/users/123")).unwrap();
    //         let headers = &http::HeaderMap::default();
    //         let request = HttpRequest::from_parts(method, url, headers).unwrap();

    //         let err = assert_resolve_err(&routes, request);
    //         assert_eq!(err.to_string(), "invalid route configuration");
    //         assert!(!err.is_temporary());
    //     }
    // }

    // #[test]
    // fn test_resolve_path_match() {
    //     let backend_one = Service::kube("web", "svc1").unwrap();
    //     let backend_two = Service::kube("web", "svc2").unwrap();

    //     let route = Route {
    //         id: Name::from_static("path-match"),
    //         hostnames: vec![Hostname::from_static("example.com").into()],
    //         ports: vec![],
    //         tags: Default::default(),
    //         rules: vec![
    //             RouteRule {
    //                 matches: vec![RouteMatch {
    //                     path: Some(PathMatch::Prefix {
    //                         value: "/users".to_string(),
    //                     }),
    //                     ..Default::default()
    //                 }],
    //                 backends: vec![BackendRef {
    //                     weight: 1,
    //                     service: backend_one.clone(),
    //                     port: Some(8910),
    //                 }],
    //                 ..Default::default()
    //             },
    //             RouteRule {
    //                 backends: vec![BackendRef {
    //                     weight: 1,
    //                     service: backend_two.clone(),
    //                     port: Some(8919),
    //                 }],
    //                 ..Default::default()
    //             },
    //         ],
    //     };

    //     let routes = StaticConfig::new(vec![route], vec![]);

    //     let url = &Url::from_str("http://example.com/test-path").unwrap();
    //     let headers = &http::HeaderMap::default();
    //     let request = HttpRequest::from_parts(&http::Method::GET, url, headers).unwrap();
    //     let resolved = assert_resolve_routes(&routes, request);

    //     // should match the fallthrough rule
    //     assert_eq!(resolved.rule, 1);
    //     assert_eq!(resolved.backend, backend_two.as_backend_id(8919));

    //     let url = Url::from_str("http://example.com/users/123").unwrap();
    //     let headers = &http::HeaderMap::default();
    //     let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();
    //     let resolved = assert_resolve_routes(&routes, request);

    //     // should match the first rule, with the path match
    //     assert_eq!(resolved.backend, backend_one.as_backend_id(8910));
    //     assert!(!resolved.route.rules[resolved.rule].matches.is_empty());

    //     let url = Url::from_str("http://example.com/users/123").unwrap();
    //     let headers = &http::HeaderMap::default();
    //     let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

    //     let resolved = assert_resolve_routes(&routes, request);
    //     // should match the first rule, with the path match
    //     assert_eq!(resolved.rule, 0);
    //     assert_eq!(resolved.backend, backend_one.as_backend_id(8910));
    // }

    // #[test]
    // fn test_resolve_query_match() {
    //     let backend_one = Service::kube("web", "svc1").unwrap();
    //     let backend_two = Service::kube("web", "svc2").unwrap();

    //     let route = Route {
    //         id: Name::from_static("query-match"),
    //         hostnames: vec![Hostname::from_static("example.com").into()],
    //         ports: vec![],
    //         tags: Default::default(),
    //         rules: vec![
    //             RouteRule {
    //                 matches: vec![RouteMatch {
    //                     query_params: vec![
    //                         QueryParamMatch::Exact {
    //                             name: "qp1".to_string(),
    //                             value: "potato".to_string(),
    //                         },
    //                         QueryParamMatch::RegularExpression {
    //                             name: "qp2".to_string(),
    //                             value: Regex::from_str("foo.*bar").unwrap(),
    //                         },
    //                     ],
    //                     ..Default::default()
    //                 }],
    //                 backends: vec![BackendRef {
    //                     weight: 1,
    //                     service: backend_one.clone(),
    //                     port: Some(8910),
    //                 }],
    //                 ..Default::default()
    //             },
    //             RouteRule {
    //                 backends: vec![BackendRef {
    //                     weight: 1,
    //                     service: backend_two.clone(),
    //                     port: Some(8919),
    //                 }],
    //                 ..Default::default()
    //             },
    //         ],
    //     };

    //     let routes = StaticConfig::new(vec![route], vec![]);

    //     let wont_match = [
    //         "http://example.com?qp1=tomato",
    //         "http://example.com?qp1=potatooo",
    //         "http://example.com?qp2=barfoo",
    //         "http://example.com?qp2=fobar",
    //         "http://example.com?qp1=potat&qp2=foobar",
    //         "http://example.com?qp1=potato&qp2=fbar",
    //     ];

    //     for url in wont_match {
    //         let url = Url::from_str(url).unwrap();
    //         let headers = &http::HeaderMap::default();
    //         let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

    //         let resolved = assert_resolve_routes(&routes, request);
    //         // should match the fallthrough rule
    //         assert_eq!(resolved.rule, 1);
    //         assert_eq!(resolved.backend, backend_two.as_backend_id(8919));
    //     }

    //     let will_match = [
    //         "http://example.com?qp1=potato&qp2=foobar",
    //         "http://example.com?qp1=potato&qp2=foobazbar",
    //         "http://example.com?qp1=potato&qp2=fooooooooooooooobar",
    //     ];

    //     for url in will_match {
    //         let url = Url::from_str(url).unwrap();
    //         let headers = &http::HeaderMap::default();
    //         let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

    //         let resolved = assert_resolve_routes(&routes, request);
    //         // should match one of the query matches
    //         assert_eq!(
    //             (resolved.rule, &resolved.backend),
    //             (0, &backend_one.as_backend_id(8910)),
    //             "should match the first rule: {url}"
    //         );
    //     }
    // }

    // #[test]
    // fn test_resolve_routes_resolves_ndots() {
    //     let backend = Service::kube("web", "svc1").unwrap();

    //     let route = Route {
    //         id: Name::from_static("ndots-match"),
    //         hostnames: vec![Hostname::from_static("example.foo.bar.com").into()],
    //         ports: vec![],
    //         tags: Default::default(),
    //         rules: vec![RouteRule {
    //             matches: vec![],
    //             backends: vec![BackendRef {
    //                 weight: 1,
    //                 service: backend.clone(),
    //                 port: Some(8910),
    //             }],
    //             ..Default::default()
    //         }],
    //     };

    //     let routes = StaticConfig::new(vec![route], vec![]);

    //     let will_match = [
    //         "http://example",
    //         "http://example.foo",
    //         "http://example.foo.bar",
    //         "http://example.foo.bar.com",
    //     ];
    //     let will_match_hostnames = vec![
    //         Hostname::from_static("foo.bar.com"),
    //         Hostname::from_static("bar.com"),
    //         Hostname::from_static("com"),
    //     ];

    //     for url in will_match {
    //         let url = crate::Url::from_str(url).unwrap();
    //         let headers = &http::HeaderMap::default();
    //         let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

    //         let resolved = resolve_routes(
    //             &routes,
    //             Trace::new(),
    //             request,
    //             None,
    //             &SearchConfig::new(3, will_match_hostnames.clone()),
    //         )
    //         .now_or_never()
    //         .unwrap()
    //         .unwrap();

    //         // should match one of the query matches
    //         assert_eq!(
    //             (resolved.rule, &resolved.backend),
    //             (0, &backend.as_backend_id(8910)),
    //             "should match the first rule: {url}"
    //         );
    //     }
    // }

    // #[test]
    // fn test_resolve_routes_resolves_ndots_no_search() {
    //     let backend = Service::kube("web", "svc1").unwrap();

    //     let will_match = [
    //         "http://example.com",
    //         "http://example.foo.com",
    //         "http://example.foo.bar.com",
    //     ];

    //     let route = Route {
    //         id: Name::from_static("ndots-match"),
    //         hostnames: vec![
    //             Hostname::from_static("example.com").into(),
    //             Hostname::from_static("example.foo.com").into(),
    //             Hostname::from_static("example.foo.bar.com").into(),
    //         ],
    //         ports: vec![],
    //         tags: Default::default(),
    //         rules: vec![RouteRule {
    //             matches: vec![],
    //             backends: vec![BackendRef {
    //                 weight: 1,
    //                 service: backend.clone(),
    //                 port: Some(8910),
    //             }],
    //             ..Default::default()
    //         }],
    //     };

    //     let routes = StaticConfig::new(vec![route], vec![]);

    //     for url in will_match {
    //         let url = crate::Url::from_str(url).unwrap();
    //         let headers = &http::HeaderMap::default();
    //         let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

    //         let resolved = resolve_routes(
    //             &routes,
    //             Trace::new(),
    //             request,
    //             None,
    //             &SearchConfig::new(3, vec![]),
    //         )
    //         .now_or_never()
    //         .unwrap()
    //         .unwrap();

    //         // should match one of the query matches
    //         assert_eq!(
    //             (resolved.rule, &resolved.backend),
    //             (0, &backend.as_backend_id(8910)),
    //             "should match the first rule: {url}"
    //         );
    //     }
    // }
}
