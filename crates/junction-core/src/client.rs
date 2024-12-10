use crate::{
    endpoints::{EndpointGroup, EndpointIter},
    load_balancer::BackendLb,
    xds::AdsClient,
    ConfigCache, Endpoint, Error, StaticConfig,
};
use futures::FutureExt;
use junction_api::{
    backend::{Backend, BackendId},
    http::{HeaderMatch, PathMatch, QueryParamMatch, Route, RouteMatch, RouteRule},
};
use rand::distributions::WeightedError;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// The strategy for udpating Routes, Backends, and Endpoints while resolving.
///
/// Resolution mode is orthogonal to client configuration mode - it's possible
/// to have a dynamic client, but to resolve an individual route by only using
/// data currently in cache.
///
/// See [Client::resolve_routes] and [Client::resolve_endpoint].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolveMode {
    /// Resolve configuration with only existing routes and backends, do not
    /// request new ones over the network.
    ///
    /// This mode does not disable push-based updates to existing routes or
    /// backends. For example, the addresses that are part of a backend or its
    /// load balancing configuration may still change during resolution.
    Static,

    /// Update configuration dynamically as part of making this request. New
    /// routes, backends, and addresses may fetched to make this request.
    Dynamic,
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
    resolve_mode: ResolveMode,

    config: Config,
}

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
    /// TOODO: should this get bundled into AdsClient? shrug emoji?
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
// of primary endpoints to cycle through on retries, and a seprate list of
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
            ads_task.connect().await.expect("xds: connection failed");
            match ads_task.run().await {
                Ok(()) => (),
                Err(e) => panic!("xds: ads client exited: unxpected error: {e}"),
            }
        });

        let dyn_config = Arc::new(DynamicConfig {
            ads_client,
            ads_task: handle,
        });

        let client = Self {
            resolve_timeout: Duration::from_secs(5),
            resolve_mode: ResolveMode::Dynamic,
            config: Config::Dynamic(dyn_config),
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

        dyn_config
            .ads_client
            .subscribe_to_backends(static_config.backends())
            // FIXME: don't panic here
            .expect("ads is overloaded. this is a bug in Junction");

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

        // FIXME: this needs a way to subscribe to DNS endpoints or specify endpoints
        // somehow?

        let config = Config::Static(static_config);
        Client { config, ..self }
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
    pub fn dump_xds(&self) -> Vec<crate::XdsConfig> {
        match self.config.ads() {
            Some(ads) => ads.iter_xds().collect(),
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

    /// Resolve an HTTP method, URL, and headers to a target backend, returning
    /// the Route that matched, the index of the rule that matched, and the
    /// backend that was chosen - to make backend choice determinstic with
    /// multiple backends, set the `JUNCTION_SEED` environment variable.
    ///
    /// This is a lower-level method that only performs the Route matching half
    /// of resolution. It's intended for debugging or querying a client for
    /// specific information. For everyday use, prefer [Client::resolve_http].
    /// [Client::resolve_http].
    pub async fn resolve_routes(
        &self,
        config_mode: ResolveMode,
        request: HttpRequest<'_>,
    ) -> crate::Result<ResolvedRoute> {
        if let (ResolveMode::Dynamic, Config::Dynamic(d)) = (config_mode, &self.config) {
            d.ads_client
                .subscribe_to_hosts([request.url.authority().to_string()])
                // FIXME: shouldn't be a panic, should be async
                .expect("ads client overloaded, this is a bug in Junction");
        }

        resolve_routes(&self.config, request).await
    }

    /// Use a resolved Route and Backend to select an endpoint for a request.
    ///
    /// This is a lower level method that only performs the Backend
    /// load-balancing half of resolution. It's intended for debugging or
    /// querying a client for specific information. For everday
    /// use, prefer [Client::resolve_http].
    pub async fn resolve_endpoint(
        &self,
        resolve_mode: ResolveMode,
        resolved: ResolvedRoute,
        request: HttpRequest<'_>,
    ) -> crate::Result<Endpoint> {
        // subscribe to backends with either dynamic endpoints or full
        // dynamic mode, since this is how we'll fetch endpoints.
        //
        // TODO: should there be a way to subscribe to endpoints here as well?
        // TODO: should we just get rid of the subscribe here and move it into
        //       the get_routes/get_backends/get_endpoints calls???
        if let (ResolveMode::Dynamic, Config::Dynamic(d) | Config::DynamicEndpoints(_, d)) =
            (resolve_mode, &self.config)
        {
            d.ads_client
                .subscribe_to_backends([resolved.backend.clone()])
                // FIXME: shouldn't be a panic, should be async
                .expect("ads client overloaded, this is a bug in Junction");
        }

        resolve_endpoint(&self.config, resolved, request).await
    }

    /// Return the endpoints currently in cache for this backend.
    ///
    /// The returned endpoints are a snapshot of what is currently in cache and
    /// will not update as new discovery information is pushed.
    pub fn get_endpoints(&self, backend: &BackendId) -> Option<EndpointIter> {
        self.config
            .get_endpoints(backend)
            .now_or_never()
            .flatten()
            .map(EndpointIter::from)
    }

    /// Resolve an HTTP method, URL, and headers into a set of [Endpoint]s.
    ///
    /// When multiple endpoints are returned, a client should send traffic to
    /// ALL of the returned endpoints because the routing policy specified
    /// that traffic should be mirrored.
    pub async fn resolve_http(
        &mut self,
        method: &http::Method,
        url: &crate::Url,
        headers: &http::HeaderMap,
    ) -> crate::Result<crate::Endpoint> {
        let request = HttpRequest::from_parts(method, url, headers)?;
        let deadline = Instant::now() + self.resolve_timeout;

        let resolve_routes = self.resolve_routes(self.resolve_mode, request.clone());
        let resolved = match tokio::time::timeout_at(deadline.into(), resolve_routes).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(Error::timed_out("timed out fetching routes")),
        };

        let resolve_endpoint = self.resolve_endpoint(self.resolve_mode, resolved, request);
        match tokio::time::timeout_at(deadline.into(), resolve_endpoint).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::timed_out("timed out fetching backends")),
        }
    }
}

/// The result of [resolving a route][Client::resolve_routes].
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    /// The resolved route.
    pub route: Arc<Route>,

    /// The index of the rule that matched the request.
    //TODO: doesn't need to be optional? remove it for the request trace anyway
    pub rule: Option<usize>,

    /// The backend selected as part of route resolution.
    pub backend: BackendId,
}

pub(crate) async fn resolve_routes(
    cache: &impl ConfigCache,
    request: HttpRequest<'_>,
) -> crate::Result<ResolvedRoute> {
    use rand::seq::SliceRandom;

    // look up the route, grabbing the first route that returns from cache.
    let matching_route = match cache.get_route(request.url.authority()).await {
        Some(route) => route,
        None => return Err(Error::no_route_matched(request.url.authority().to_string())),
    };

    // if we got here, we have resolved to a list of routes
    let Some((matching_rule_idx, matching_rule)) = find_matching_rule(
        &matching_route,
        request.method,
        request.url,
        request.headers,
    ) else {
        return Err(Error::no_rule_matched(matching_route.id.clone()));
    };

    // pick a target at random from the list, respecting weights.
    let weighted_backend = &crate::rand::with_thread_rng(|rng| {
        matching_rule.backends.choose_weighted(rng, |wc| wc.weight)
    });

    let backend_ref = match weighted_backend {
        Ok(backend_ref) => backend_ref,
        Err(WeightedError::NoItem) => {
            return Err(Error::invalid_route(
                "route has no backends",
                matching_route.id.clone(),
                matching_rule_idx,
            ))
        }
        Err(_) => {
            return Err(Error::invalid_route(
                "backends weights are invalid: total weights must be greater than zero",
                matching_route.id.clone(),
                matching_rule_idx,
            ))
        }
    };

    let backend = backend_ref.into_backend_id(request.url.default_port());
    Ok(ResolvedRoute {
        route: matching_route.clone(),
        rule: Some(matching_rule_idx),
        backend,
    })
}

async fn resolve_endpoint(
    cache: &impl ConfigCache,
    resolved: ResolvedRoute,
    request: HttpRequest<'_>,
) -> crate::Result<Endpoint> {
    let Some(backend) = cache.get_backend(&resolved.backend).await else {
        return Err(Error::no_backend(
            resolved.route.id.clone(),
            resolved.rule,
            resolved.backend,
        ));
    };

    let Some(endpoints) = cache.get_endpoints(&resolved.backend).await else {
        return Err(Error::no_reachable_endpoints(
            resolved.route.id.clone(),
            backend.config.id.clone(),
        ));
    };

    let endpoint = backend
        .load_balancer
        .load_balance(request.url, request.headers, &endpoints);
    let Some(endpoint) = endpoint else {
        return Err(Error::no_reachable_endpoints(
            resolved.route.id.clone(),
            resolved.backend,
        ));
    };

    let url = request.url.clone();
    let (timeouts, retry) = match resolved.rule {
        Some(idx) => {
            let rule = &resolved.route.rules[idx];
            (rule.timeouts.clone(), rule.retry.clone())
        }
        None => (None, None),
    };

    Ok(crate::Endpoint {
        url,
        timeouts,
        retry,
        address: *endpoint,
    })
}

//FIXME(routing): picking between these is way more complicated than finding the
//first match
fn find_matching_rule<'a>(
    route: &'a Route,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Option<(usize, &'a RouteRule)> {
    let rule_idx = route
        .rules
        .iter()
        .position(|rule| is_route_rule_match(rule, method, url, headers))?;

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

// TODO: thorough tests for matching

#[cfg(test)]
mod test {
    use crate::Url;
    use junction_api::{http::BackendRef, Hostname, Name, Regex, Service};
    use std::str::FromStr;

    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn assert_send_sync() {
        assert_send::<HttpRequest<'_>>();
        assert_sync::<HttpRequest<'_>>();
    }

    #[track_caller]
    fn assert_resolve_routes(cache: &impl ConfigCache, request: HttpRequest<'_>) -> ResolvedRoute {
        resolve_routes(cache, request)
            .now_or_never()
            .unwrap()
            .unwrap()
    }

    #[track_caller]
    fn assert_resolve_err(cache: &impl ConfigCache, request: HttpRequest<'_>) -> crate::Error {
        resolve_routes(cache, request)
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
        assert!(err
            .to_string()
            .contains("no routing rules matched the request"));
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
        assert_eq!(resolved.rule, Some(1));
        assert_eq!(resolved.backend, backend_two.as_backend_id(8919));

        let url = Url::from_str("http://example.com/users/123").unwrap();
        let headers = &http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();
        let resolved = assert_resolve_routes(&routes, request);

        // should match the first rule, with the path match
        assert_eq!(resolved.backend, backend_one.as_backend_id(8910));
        assert!(!resolved.route.rules[resolved.rule.unwrap()]
            .matches
            .is_empty());

        let url = Url::from_str("http://example.com/users/123").unwrap();
        let headers = &http::HeaderMap::default();
        let request = HttpRequest::from_parts(&http::Method::GET, &url, headers).unwrap();

        let resolved = assert_resolve_routes(&routes, request);
        // should match the first rule, with the path match
        assert_eq!(resolved.rule, Some(0));
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
            assert_eq!(resolved.rule, Some(1));
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
                (Some(0), &backend_one.as_backend_id(8910)),
                "should match the first rule: {url}"
            );
        }
    }
}
