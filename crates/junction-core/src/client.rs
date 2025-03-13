use crate::{
    dns,
    endpoints::{EndpointGroup, EndpointIter},
    error::Trace,
    load_balancer::BackendLb,
    xds::AdsClient,
    ConfigCache, Endpoint, Error, StaticConfig,
};
use futures::{stream::FuturesOrdered, FutureExt, StreamExt};
use junction_api::{
    backend::{Backend, BackendId},
    http::{HeaderMatch, PathMatch, QueryParamMatch, Route, RouteMatch, RouteRule},
    Hostname,
};
use rand::distributions::WeightedError;
use serde::Deserialize;
use std::{
    borrow::Cow,
    time::{Duration, Instant},
};
use std::{net::SocketAddr, sync::Arc};

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
    ) -> crate::Result<Self> {
        Ok(Self {
            method,
            url,
            headers,
        })
    }
}

/// The result of resolving a route (see [Client::resolve_route]).
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    /// The resolved route.
    pub route: Arc<Route>,

    /// The index of the rule that matched the request.
    //TODO: doesn't need to be optional? remove it for the request trace anyway
    pub rule: usize,

    /// The backend selected as part of route resolution.
    pub backend: BackendId,

    /// smuggle the request trace through here
    trace: Trace,
}

/// The context required to select an address from a backend. Includes the URL
/// and headers from an outgoing request.
#[derive(Debug, Clone)]
pub struct LbContext<'a> {
    url: &'a crate::Url,

    headers: &'a http::HeaderMap,

    previous_addrs: &'a [SocketAddr],

    /// smuggle the request trace through here
    trace: Trace,
}

impl<'a> LbContext<'a> {
    // unused, allowed so that we can make select_endpoint public without exposing Trace
    #[allow(unused)]
    pub fn from_parts(url: &'a crate::Url, headers: &'a http::HeaderMap) -> Self {
        let trace = Trace::new();
        Self {
            url,
            headers,
            previous_addrs: &[],
            trace,
        }
    }

    fn new(trace: Trace, url: &'a crate::Url, headers: &'a http::HeaderMap) -> Self {
        Self {
            url,
            headers,
            previous_addrs: &[],
            trace,
        }
    }
}

/// The result of selecting an endpoint (see [Client::select_endpoint]).
pub struct SelectedEndpoint {
    /// The selected endpoint address
    pub addr: SocketAddr,

    // smuggle trace data back out
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

    // junction data
    config: Config,
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
// the entire static config thing might be a mistake and worth revisting - we
// could insert resources into cache and let multiple clients in the same process
// all see static resources. is that good? idk. we already have to_xds() on every
// resource type, and it would remove a lot of code.
//
// revisit this when we hit the problem of static bootstrapping/fallback for
// clients .
#[derive(Clone)]
enum Config {
    Static(Arc<StaticConfig>),
    DynamicEndpoints(Arc<StaticConfig>, Arc<DynamicConfig>),
    Dynamic(Arc<DynamicConfig>),
}

struct DynamicConfig {
    ads_client: AdsClient,

    /// a the shared handle to the task that's actually running the client in
    /// the background. should not drop until every active client drops.
    ///
    /// TODO: should this get bundled into AdsClient? shrug emoji?
    #[allow(unused)]
    ads_task: tokio::task::JoinHandle<()>,
}

impl Config {
    fn ads(&self) -> Option<&AdsClient> {
        match self {
            Config::Static(_) => None,
            Config::DynamicEndpoints(_, d) | Config::Dynamic(d) => Some(&d.ads_client),
        }
    }
}

impl ConfigCache for Config {
    async fn get_route<S: AsRef<str>>(&self, host: S) -> Option<Arc<Route>> {
        match &self {
            Config::Static(s) => s.get_route(host).await,
            Config::DynamicEndpoints(s, _) => s.get_route(host).await,
            Config::Dynamic(d) => d.ads_client.get_route(host).await,
        }
    }

    async fn get_backend(&self, target: &BackendId) -> Option<Arc<BackendLb>> {
        match &self {
            Config::Static(s) => s.get_backend(target).await,
            Config::DynamicEndpoints(s, _) => s.get_backend(target).await,
            Config::Dynamic(d) => d.ads_client.get_backend(target).await,
        }
    }

    async fn get_endpoints(&self, backend: &BackendId) -> Option<Arc<EndpointGroup>> {
        match &self {
            Config::Static(s) => s.get_endpoints(backend).await,
            Config::DynamicEndpoints(_, d) => d.ads_client.get_endpoints(backend).await,
            Config::Dynamic(d) => d.ads_client.get_endpoints(backend).await,
        }
    }
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
        let (ads_client, mut ads_task) = AdsClient::build(address, node_id, cluster).unwrap();

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
        let config = Config::Dynamic(Arc::new(DynamicConfig {
            ads_client,
            ads_task: handle,
        }));
        let client = Self {
            resolve_timeout: Duration::from_secs(5),
            config,
            search_config,
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
    pub fn with_static_config(self, routes: Vec<Route>, backends: Vec<Backend>) -> Client {
        let static_config = Arc::new(StaticConfig::with_inferred(routes, backends));

        let dyn_config = match &self.config {
            Config::Static(_) => panic!("can't use dynamic endpoints with a fully static client"),
            Config::DynamicEndpoints(_, d) => Arc::clone(d),
            Config::Dynamic(d) => Arc::clone(d),
        };

        let config = Config::DynamicEndpoints(static_config, dyn_config);
        Client { config, ..self }
    }

    /// Construct a client that uses fully static configuration and does not
    /// connect to a control plane at all.
    ///
    /// This is intended to be used to test configuration in controlled settings
    /// or to use Junction an offline mode. Once a client has been converted to
    /// fully static, it's not possible to convert it back to using dynamic
    /// discovery data.
    pub fn with_static_endpoints(self, routes: Vec<Route>, backends: Vec<Backend>) -> Client {
        let static_config = Arc::new(StaticConfig::with_inferred(routes, backends));
        let config = Config::Static(static_config);
        Client { config, ..self }
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

        let request = HttpRequest::from_parts(method, url, headers)?;

        let resolved = self.resolve_route(request, Some(deadline)).await?;

        let lb_context = LbContext::new(resolved.trace, url, headers);
        let selected = self
            .select_endpoint(&resolved.backend, lb_context, Some(deadline))
            .await?;

        let address = selected.addr;
        let trace = selected.trace;
        let (timeouts, retry) = {
            let rule = &resolved.route.rules[resolved.rule];
            (rule.timeouts.clone(), rule.retry.clone())
        };

        Ok(Endpoint {
            method: method.clone(),
            url: url.clone(),
            headers: headers.clone(),
            address,
            timeouts,
            retry,
            backend: resolved.backend,
            trace,
            previous_addrs: vec![],
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
        // TODO: track response stats

        // if there's no reason to pick a new endpoint, just return the existing one as-is
        if response.is_ok() || !endpoint.should_retry(response) {
            return Ok(endpoint);
        }

        // redo endpoint selection
        // FIXME: real deadline here
        let deadline = Instant::now() + self.resolve_timeout;
        let lb_context = LbContext {
            url: &endpoint.url,
            headers: &endpoint.headers,
            previous_addrs: &endpoint.previous_addrs,
            trace: endpoint.trace,
        };
        let next = self
            .select_endpoint(&endpoint.backend, lb_context, Some(deadline))
            .await?;
        let address = next.addr;
        let trace = next.trace;

        // track address history
        let mut previous_addrs = endpoint.previous_addrs;
        previous_addrs.push(endpoint.address);

        Ok(Endpoint {
            address,
            trace,
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
    pub async fn resolve_route(
        &self,
        request: HttpRequest<'_>,
        deadline: Option<Instant>,
    ) -> crate::Result<ResolvedRoute> {
        let trace = Trace::new();
        resolve_routes(&self.config, trace, request, deadline, &self.search_config).await
    }

    /// Select an endpoint address for this backend from the set of currently
    /// available endpoints.
    ///
    /// This is a lower level method that only performs part of route
    /// resolution, and is intended for debugging and testing. For everyday use,
    /// prefer [Client::resolve_http].
    pub async fn select_endpoint(
        &self,
        backend: &BackendId,
        ctx: LbContext<'_>,
        deadline: Option<Instant>,
    ) -> crate::Result<SelectedEndpoint> {
        select_endpoint(&self.config, backend, ctx, deadline).await
    }

    /// Start a gRPC CSDS server on the given port. To run the server, you must
    /// `await` this future.
    ///
    /// For static clients, this does nothing.
    pub async fn csds_server(self, port: u16) -> Result<(), tonic::transport::Error> {
        match self.config.ads() {
            Some(ads) => ads.csds_server(port).await,
            None => std::future::pending().await,
        }
    }

    /// Dump the client's current cache of xDS resources, as fetched from the
    /// config server.
    ///
    /// This is a programmatic view of the same data that you can fetch over
    /// gRPC by starting a [Client::csds_server].
    pub fn dump_xds(&self, not_found: bool) -> Vec<crate::XdsConfig> {
        match self.config.ads() {
            Some(ads) => {
                if not_found {
                    ads.iter_xds().collect()
                } else {
                    ads.iter_xds().filter(|c| c.xds.is_some()).collect()
                }
            }
            None => Vec::new(),
        }
    }

    /// Dump xDS resources that failed to update. This is a view of the data
    /// returned by [Client::dump_xds] that only contains resources with
    /// errors.
    pub fn dump_xds_errors(&self) -> Vec<crate::XdsConfig> {
        match self.config.ads() {
            Some(ads) => ads
                .iter_xds()
                .filter(|xds| xds.last_error.is_some())
                .collect(),
            None => Vec::new(),
        }
    }

    /// Dump the Client's current table of [Route]s, merging together any
    /// default routes and remotely fetched routes the same way the client would
    /// when resolving endpoints.
    pub fn dump_routes(&self) -> Vec<Arc<Route>> {
        match &self.config {
            Config::Static(c) | Config::DynamicEndpoints(c, _) => c.routes.clone(),
            Config::Dynamic(d) => d.ads_client.iter_routes().collect(),
        }
    }

    /// Dump the Client's current table of [BackendLb]s, merging together any
    /// default configuration and remotely fetched config the same way the
    /// client would when resolving endpoints.
    pub fn dump_backends(&self) -> Vec<Arc<BackendLb>> {
        match &self.config {
            Config::Static(c) | Config::DynamicEndpoints(c, _) => {
                c.backends.values().cloned().collect()
            }
            Config::Dynamic(d) => d.ads_client.iter_backends().collect(),
        }
    }

    /// Return the endpoints currently in cache for this backend.
    ///
    /// The returned endpoints are a snapshot of what is currently in cache and
    /// will not update as new discovery information is pushed.
    pub fn dump_endpoints(&self, backend: &BackendId) -> Option<EndpointIter> {
        self.config
            .get_endpoints(backend)
            .now_or_never()
            .flatten()
            .map(EndpointIter::from)
    }
}

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

pub(crate) async fn resolve_routes(
    cache: &impl ConfigCache,
    mut trace: Trace,
    request: HttpRequest<'_>,
    deadline: Option<Instant>,
    search_config: &SearchConfig,
) -> crate::Result<ResolvedRoute> {
    use rand::seq::SliceRandom;

    let uris_to_search = search(search_config, request.url);
    assert!(
        !uris_to_search.is_empty(),
        "URI search is empty, this is a bug in Junction."
    );

    let mut futures_ordered = FuturesOrdered::new();
    for url in uris_to_search {
        futures_ordered.push_back(cache.get_route(url.authority().to_string()));
    }

    // NB[pt): two potential surprises below:
    //  1. we do not surface any errors that occur subsequent to the first success in the list of
    //     routes.
    //  2. we rely on .next() called on FuturesOrdered to start all the futures contained in
    //     futures_ordered. We expect all routes to be checked in parallel depending on load in
    //     tokio.
    let msg = "timed out fetching route";
    let route = loop {
        match with_deadline!(futures_ordered.next(), deadline, msg, trace) {
            Some(Some(route)) => break route,
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

    trace.lookup_route(&route);

    // match the request against the list of RouteRules that are part of this
    // request. the hostname and port of the request have already matched but we
    // need to match headers/url params/method and so on.
    let (rule, matching_rule) = match find_matching_rule(&route, request.clone()) {
        Some((idx, r)) => (idx, r),
        None => return Err(Error::no_rule_matched(route.id.clone(), trace)),
    };
    trace.matched_rule(
        rule,
        route.rules.get(rule).and_then(|rule| rule.name.as_ref()),
    );

    // pick a target at random from the list, respecting weights. if there are
    // no backends listed we should blackhole here.
    let weighted_backend = &crate::rand::with_thread_rng(|rng| {
        matching_rule.backends.choose_weighted(rng, |wc| wc.weight)
    });
    let backend_ref = match weighted_backend {
        Ok(backend_ref) => backend_ref,
        Err(WeightedError::NoItem) => {
            // TODO: should this just return a special endpoint that 500s?
            return Err(Error::invalid_route(
                "route has no backends",
                route.id.clone(),
                rule,
                trace,
            ));
        }
        Err(_) => {
            return Err(Error::invalid_route(
                "backends weights are invalid: total weights must be greater than zero",
                route.id.clone(),
                rule,
                trace,
            ))
        }
    };
    let backend = backend_ref.into_backend_id(request.url.default_port());
    trace.select_backend(&backend);

    Ok(ResolvedRoute {
        route,
        rule,
        backend,
        trace,
    })
}

async fn select_endpoint(
    cache: &impl ConfigCache,
    backend: &BackendId,
    mut ctx: LbContext<'_>,
    deadline: Option<Instant>,
) -> crate::Result<SelectedEndpoint> {
    // start the next trace phase
    ctx.trace.start_endpoint_selection();

    // lookup backend and endpoints
    //
    // these lookups are done sequentially, even though they could be raced, so
    // that we can tell what step a lookup timed out on. we're assuming that
    // this is okay because in the background fetching a backend for a cache
    // will also trigger fetching its endpoints and parallelism here won't do
    // much for us.
    let blb = with_deadline!(
        cache.get_backend(backend),
        deadline,
        "timed out fetching backend",
        ctx.trace,
    );
    let Some(blb) = blb else {
        return Err(Error::no_backend(backend.clone(), ctx.trace));
    };
    ctx.trace.lookup_backend(backend);

    let endpoints = with_deadline!(
        cache.get_endpoints(backend),
        deadline,
        "timed out fetching endpoints",
        ctx.trace,
    );
    let Some(endpoints) = endpoints else {
        return Err(Error::no_reachable_endpoints(backend.clone(), ctx.trace));
    };
    ctx.trace.lookup_endpoints(backend);

    // load balance.
    //
    // no trace is done here, the load balancer impls stamp the traces themselves
    let addr = blb.load_balancer.load_balance(
        &mut ctx.trace,
        &endpoints,
        ctx.url,
        ctx.headers,
        ctx.previous_addrs,
    );
    let Some(addr) = addr else {
        return Err(Error::no_reachable_endpoints(backend.clone(), ctx.trace));
    };

    Ok(SelectedEndpoint {
        addr: *addr,
        trace: ctx.trace,
    })
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

//FIXME(routing): picking between these is way more complicated than finding the
//first match
fn find_matching_rule<'a>(
    route: &'a Route,
    request: HttpRequest<'_>,
) -> Option<(usize, &'a RouteRule)> {
    let rule_idx = route
        .rules
        .iter()
        .position(|rule| is_route_rule_match(rule, request.method, request.url, request.headers))?;

    let rule = &route.rules[rule_idx];
    Some((rule_idx, rule))
}

pub fn is_route_rule_match(
    rule: &RouteRule,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> bool {
    if rule.matches.is_empty() {
        return true;
    }
    rule.matches
        .iter()
        .any(|m| is_route_match_match(m, method, url, headers))
}

pub fn is_route_match_match(
    rule: &RouteMatch,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> bool {
    let mut method_matches = true;
    if let Some(rule_method) = &rule.method {
        method_matches = rule_method.eq(&method.to_string());
    }

    let mut path_matches = true;
    if let Some(rule_path) = &rule.path {
        path_matches = match &rule_path {
            PathMatch::Exact { value } => value == url.path(),
            PathMatch::Prefix { value } => url.path().starts_with(value),
            PathMatch::RegularExpression { value } => value.is_match(url.path()),
        }
    }

    let headers_matches = rule.headers.iter().all(|m| is_header_match(m, headers));
    let qp_matches = rule
        .query_params
        .iter()
        .all(|m| is_query_params_match(m, url.query()));

    method_matches && path_matches && headers_matches && qp_matches
}

pub fn is_header_match(rule: &HeaderMatch, headers: &http::HeaderMap) -> bool {
    let Some(header_val) = headers.get(rule.name()) else {
        return false;
    };
    let Ok(header_val) = header_val.to_str() else {
        return false;
    };
    rule.is_match(header_val)
}

pub fn is_query_params_match(rule: &QueryParamMatch, query: Option<&str>) -> bool {
    let Some(query) = query else {
        return false;
    };
    for (param, value) in form_urlencoded::parse(query.as_bytes()) {
        if param == rule.name() {
            return rule.is_match(&value);
        }
    }
    false
}

/// generate a URL search path for this url.
///
/// the resturned Vec will always contain either:
///
/// - a single element, a ref to the original URL
///
/// - `search.len() + 1` elements, where the first element is the original
///    URl and the rest of the entries are the result of appending the URL's
///    hostname to the suffixes in search_config.search. the order of the suffixes in
///    search is preserved.
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
    use junction_api::{http::BackendRef, Hostname, Name, Regex, Service};
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

    #[track_caller]
    fn assert_resolve_routes(cache: &impl ConfigCache, request: HttpRequest<'_>) -> ResolvedRoute {
        resolve_routes(cache, Trace::new(), request, None, &SearchConfig::default())
            .now_or_never()
            .unwrap()
            .unwrap()
    }

    #[track_caller]
    fn assert_resolve_err(cache: &impl ConfigCache, request: HttpRequest<'_>) -> crate::Error {
        resolve_routes(cache, Trace::new(), request, None, &SearchConfig::default())
            .now_or_never()
            .unwrap()
            .unwrap_err()
    }

    #[test]
    fn test_resolve_passthrough_route() {
        let svc = Service::dns("example.com").unwrap();

        let routes = StaticConfig::new(
            vec![Route::passthrough_route(
                Name::from_static("example"),
                svc.clone(),
            )],
            vec![],
        );

        // check with no port
        let url = Url::from_str("http://example.com/test-path").unwrap();
        let headers = http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

        let resolved = assert_resolve_routes(&routes, request);
        assert_eq!(resolved.backend, svc.as_backend_id(80));

        // check with explicit ports
        for port in [443, 8008] {
            let url = Url::from_str(&format!("http://example.com:{port}/test-path")).unwrap();
            let headers = http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

            let resolved = assert_resolve_routes(&routes, request);
            assert_eq!(resolved.backend, svc.as_backend_id(port));
        }
    }

    #[test]
    fn test_resolve_route_no_rules() {
        let route = Route {
            id: Name::from_static("no-rules"),
            hostnames: vec![Hostname::from_static("example.com").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![],
        };

        let routes = StaticConfig::new(vec![route], vec![]);

        let url = Url::from_str("http://example.com:3214/users/123").unwrap();
        let headers = http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

        let err = assert_resolve_err(&routes, request);
        assert!(err.to_string().contains("no rules matched the request"));
        assert!(!err.is_temporary());
    }

    #[test]
    fn test_resolve_route_no_rules_with_search_config() {
        let route = Route {
            id: Name::from_static("no-rules"),
            hostnames: vec![Hostname::from_static("example.com").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![],
        };

        let routes = StaticConfig::new(vec![route], vec![]);

        let url = Url::from_str("http://example.com:3214/users/123").unwrap();
        let headers = http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, &headers).unwrap();

        let err = resolve_routes(
            &routes,
            Trace::new(),
            request,
            None,
            &SearchConfig::new(2, vec![Hostname::from_static("example.com")]),
        )
        .now_or_never()
        .unwrap()
        .unwrap_err();

        assert!(err.to_string().contains("no rules matched the request"));
        assert!(!err.is_temporary());
    }

    #[test]
    fn test_resolve_route_no_backends() {
        let route = Route {
            id: Name::from_static("no-backends"),
            hostnames: vec![Hostname::from_static("example.com").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::Prefix {
                        value: "".to_string(),
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let routes = StaticConfig::new(vec![route], vec![]);

        for port in [80, 7887] {
            let method = &http::Method::GET;
            let url = &Url::from_str(&format!("http://example.com:{port}/users/123")).unwrap();
            let headers = &http::HeaderMap::default();
            let request = HttpRequest::from_parts(method, url, headers).unwrap();

            let err = assert_resolve_err(&routes, request);
            assert_eq!(err.to_string(), "invalid route configuration");
            assert!(!err.is_temporary());
        }
    }

    #[test]
    fn test_resolve_path_match() {
        let backend_one = Service::kube("web", "svc1").unwrap();
        let backend_two = Service::kube("web", "svc2").unwrap();

        let route = Route {
            id: Name::from_static("path-match"),
            hostnames: vec![Hostname::from_static("example.com").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/users".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: backend_one.clone(),
                        port: Some(8910),
                    }],
                    ..Default::default()
                },
                RouteRule {
                    backends: vec![BackendRef {
                        weight: 1,
                        service: backend_two.clone(),
                        port: Some(8919),
                    }],
                    ..Default::default()
                },
            ],
        };

        let routes = StaticConfig::new(vec![route], vec![]);

        let url = &Url::from_str("http://example.com/test-path").unwrap();
        let headers = &http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, url, headers).unwrap();
        let resolved = assert_resolve_routes(&routes, request);

        // should match the fallthrough rule
        assert_eq!(resolved.rule, 1);
        assert_eq!(resolved.backend, backend_two.as_backend_id(8919));

        let url = Url::from_str("http://example.com/users/123").unwrap();
        let headers = &http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();
        let resolved = assert_resolve_routes(&routes, request);

        // should match the first rule, with the path match
        assert_eq!(resolved.backend, backend_one.as_backend_id(8910));
        assert!(!resolved.route.rules[resolved.rule].matches.is_empty());

        let url = Url::from_str("http://example.com/users/123").unwrap();
        let headers = &http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

        let resolved = assert_resolve_routes(&routes, request);
        // should match the first rule, with the path match
        assert_eq!(resolved.rule, 0);
        assert_eq!(resolved.backend, backend_one.as_backend_id(8910));
    }

    #[test]
    fn test_resolve_query_match() {
        let backend_one = Service::kube("web", "svc1").unwrap();
        let backend_two = Service::kube("web", "svc2").unwrap();

        let route = Route {
            id: Name::from_static("query-match"),
            hostnames: vec![Hostname::from_static("example.com").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    matches: vec![RouteMatch {
                        query_params: vec![
                            QueryParamMatch::Exact {
                                name: "qp1".to_string(),
                                value: "potato".to_string(),
                            },
                            QueryParamMatch::RegularExpression {
                                name: "qp2".to_string(),
                                value: Regex::from_str("foo.*bar").unwrap(),
                            },
                        ],
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: backend_one.clone(),
                        port: Some(8910),
                    }],
                    ..Default::default()
                },
                RouteRule {
                    backends: vec![BackendRef {
                        weight: 1,
                        service: backend_two.clone(),
                        port: Some(8919),
                    }],
                    ..Default::default()
                },
            ],
        };

        let routes = StaticConfig::new(vec![route], vec![]);

        let wont_match = [
            "http://example.com?qp1=tomato",
            "http://example.com?qp1=potatooo",
            "http://example.com?qp2=barfoo",
            "http://example.com?qp2=fobar",
            "http://example.com?qp1=potat&qp2=foobar",
            "http://example.com?qp1=potato&qp2=fbar",
        ];

        for url in wont_match {
            let url = Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

            let resolved = assert_resolve_routes(&routes, request);
            // should match the fallthrough rule
            assert_eq!(resolved.rule, 1);
            assert_eq!(resolved.backend, backend_two.as_backend_id(8919));
        }

        let will_match = [
            "http://example.com?qp1=potato&qp2=foobar",
            "http://example.com?qp1=potato&qp2=foobazbar",
            "http://example.com?qp1=potato&qp2=fooooooooooooooobar",
        ];

        for url in will_match {
            let url = Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

            let resolved = assert_resolve_routes(&routes, request);
            // should match one of the query matches
            assert_eq!(
                (resolved.rule, &resolved.backend),
                (0, &backend_one.as_backend_id(8910)),
                "should match the first rule: {url}"
            );
        }
    }

    #[test]
    fn test_resolve_routes_resolves_ndots() {
        let backend = Service::kube("web", "svc1").unwrap();

        let route = Route {
            id: Name::from_static("ndots-match"),
            hostnames: vec![Hostname::from_static("example.foo.bar.com").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![],
                backends: vec![BackendRef {
                    weight: 1,
                    service: backend.clone(),
                    port: Some(8910),
                }],
                ..Default::default()
            }],
        };

        let routes = StaticConfig::new(vec![route], vec![]);

        let will_match = [
            "http://example",
            "http://example.foo",
            "http://example.foo.bar",
            "http://example.foo.bar.com",
        ];
        let will_match_hostnames = vec![
            Hostname::from_static("foo.bar.com"),
            Hostname::from_static("bar.com"),
            Hostname::from_static("com"),
        ];

        for url in will_match {
            let url = crate::Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

            let resolved = resolve_routes(
                &routes,
                Trace::new(),
                request,
                None,
                &SearchConfig::new(3, will_match_hostnames.clone()),
            )
            .now_or_never()
            .unwrap()
            .unwrap();

            // should match one of the query matches
            assert_eq!(
                (resolved.rule, &resolved.backend),
                (0, &backend.as_backend_id(8910)),
                "should match the first rule: {url}"
            );
        }
    }

    #[test]
    fn test_resolve_routes_resolves_ndots_no_search() {
        let backend = Service::kube("web", "svc1").unwrap();

        let will_match = [
            "http://example.com",
            "http://example.foo.com",
            "http://example.foo.bar.com",
        ];

        let route = Route {
            id: Name::from_static("ndots-match"),
            hostnames: vec![
                Hostname::from_static("example.com").into(),
                Hostname::from_static("example.foo.com").into(),
                Hostname::from_static("example.foo.bar.com").into(),
            ],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![],
                backends: vec![BackendRef {
                    weight: 1,
                    service: backend.clone(),
                    port: Some(8910),
                }],
                ..Default::default()
            }],
        };

        let routes = StaticConfig::new(vec![route], vec![]);

        for url in will_match {
            let url = crate::Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();
            let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

            let resolved = resolve_routes(
                &routes,
                Trace::new(),
                request,
                None,
                &SearchConfig::new(3, vec![]),
            )
            .now_or_never()
            .unwrap()
            .unwrap();

            // should match one of the query matches
            assert_eq!(
                (resolved.rule, &resolved.backend),
                (0, &backend.as_backend_id(8910)),
                "should match the first rule: {url}"
            );
        }
    }
}
