use crate::{load_balancer::BackendLb, xds::AdsClient, ConfigCache, Endpoint, Error, StaticConfig};
use junction_api::{
    backend::Backend,
    http::{HeaderMatch, PathMatch, QueryParamMatch, Route, RouteMatch, RouteRule},
    BackendId, Target, VirtualHost,
};
use rand::distributions::WeightedError;
use std::time::Duration;
use std::{collections::BTreeSet, sync::Arc};
use std::{future::Future, str::FromStr};

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
    ads: AdsClient,
    _ads_task: Arc<tokio::task::JoinHandle<()>>,
    defaults: StaticConfig,
}

// NOTE: we've largely been ignoring this and trying to make Route and Backend
// correct-by-construction. the hook is still here as a reminder to check back
// on whether this is true before we release a stable version.
fn validate_defaults(_: &[Route], _: &[Backend]) -> crate::Result<()> {
    Ok(())
}

// FIXME: Vec<Endpoints> is probably the wrong thing to return from all our
// resolve methods. We probably need a struct that has something like a list
// of primary endpoints to cycle through on retries, and a seprate list of
// endpoints to mirror traffic to. Figure that out once we support mirroring.

/// How to resolve routes and endpoints.
///
/// See [Client::resolve_routes].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigMode {
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

impl Client {
    /// Build a new client, spawning a new ADS client in the background.
    ///
    /// This method creates a new ADS client and ADS connection. Data fetched
    /// over ADS won't be shared with existing clients. To create a client that
    /// shares with an existing cache, call [Client::clone] on an existing
    /// client.
    ///
    /// This function assumes that you're currently running the context of a
    /// `tokio` runtime and spawns background tasks.
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
            ads_task.connect().await.expect("xds: connection failed");
            match ads_task.run().await {
                Ok(()) => (),
                Err(e) => panic!("xds: ads client exited: unxpected error: {e}"),
            }
        });

        let client = Self {
            ads,
            _ads_task: Arc::new(handle),
            defaults: StaticConfig::default(),
        };

        Ok(client)
    }

    /// Set default `routes` and `backends` for this client.
    ///
    /// The client will continue to use the same dynamic configuration cache it
    /// previously used.
    pub fn with_defaults(
        self,
        default_routes: Vec<Route>,
        default_backends: Vec<Backend>,
    ) -> crate::Result<Client> {
        validate_defaults(&default_routes, &default_backends)?;
        let defaults = StaticConfig::new(default_routes, default_backends);
        self.subscribe_to_defaults(&defaults);

        Ok(Client { defaults, ..self })
    }

    fn subscribe_to_defaults(&self, defaults: &StaticConfig) {
        let mut vhosts = Vec::with_capacity(defaults.routes.len());
        let mut backends = Vec::with_capacity(defaults.routes.len());
        for (target, route) in &defaults.routes {
            vhosts.push(target.clone());

            for rule in &route.rules {
                for backend in &rule.backends {
                    backends.push(backend.backend.clone());
                }
            }
        }

        self.ads.subscribe_to_vhosts(vhosts).unwrap();

        let default_backends = defaults.backends.keys().cloned();
        self.ads
            .subscribe_to_backends(backends.into_iter().chain(default_backends))
            .unwrap();
    }

    /// Start a gRPC CSDS server on the given port.
    ///
    /// To run the server, you must `await` this future.
    pub fn csds_server(
        &self,
        port: u16,
    ) -> impl Future<Output = Result<(), tonic::transport::Error>> {
        self.ads.csds_server(port)
    }

    /// Dump the client's current cache of xDS resources, as fetched from the
    /// config server.
    ///
    /// This is a programmatic view of the same data that you can fetch over
    /// gRPC by starting a [Client::csds_server].
    pub fn dump_xds(&self) -> impl Iterator<Item = crate::XdsConfig> + '_ {
        self.ads.iter_xds()
    }

    /// Dump the Client's current table of [Route]s, merging together any
    /// default routes and remotely fetched routes the same way the client would
    /// when resolving endpoints.
    pub fn dump_routes(&self) -> Vec<Arc<Route>> {
        let mut routes = vec![];
        let mut defaults: BTreeSet<_> = self.defaults.routes.keys().collect();

        for route in self.ads.iter_routes() {
            let route = if is_generated_route(&route) {
                self.defaults
                    .routes
                    .get(&route.vhost)
                    .cloned()
                    .unwrap_or(route)
            } else {
                route
            };
            defaults.remove(&route.vhost);
            routes.push(route);
        }

        for route_name in defaults {
            // safety: this started as the key set of default_routes
            routes.push(self.defaults.routes.get(route_name).unwrap().clone());
        }

        routes
    }

    /// Dump the Client's current table of [BackendLb]s, merging together any
    /// default configuration and remotely fetched config the same way the
    /// client would when resolving endpoints.
    pub fn dump_backends(&self) -> Vec<Arc<BackendLb>> {
        let mut backends = vec![];
        let mut defaults: BTreeSet<_> = self.defaults.backends.keys().collect();

        for backend in self.ads.iter_backends() {
            let backend = if backend.config.lb.is_unspecified() {
                self.defaults
                    .backends
                    .get(&backend.config.id)
                    .cloned()
                    .unwrap_or(backend)
            } else {
                backend
            };

            defaults.remove(&backend.config.id);
            backends.push(backend);
        }

        for backend_name in defaults {
            backends.push(self.defaults.backends.get(backend_name).unwrap().clone());
        }

        backends
    }

    /// Resolve an HTTP method, URL, and headers to a target backend, returning
    /// the Route that matched, the index of the rule that matched, and the
    /// backend that the [Target] that was selected.
    ///
    /// This is a lower-level method that only performs the Route matching half
    /// of full resolution. It's intended for debugging or querying a client
    /// for specific information. For everyday use, prefer
    /// [Client::resolve_http].
    pub fn resolve_routes(
        &self,
        config_mode: ConfigMode,
        request: HttpRequest<'_>,
    ) -> crate::Result<ResolvedRoute> {
        match config_mode {
            ConfigMode::Dynamic => {
                // in dynamic mode, use every target as an ads client
                // subscription. this pins every target to the ads cache.
                resolve_routes(
                    &self.ads,
                    &self.defaults,
                    request,
                    |vhosts: &[VirtualHost]| {
                        self.ads
                            .subscribe_to_vhosts(vhosts.iter().cloned())
                            .unwrap();
                    },
                )
            }
            _ => {
                // otherwise just no-op the subscribe fn and use the existing
                // ads cache/defaults
                resolve_routes(&self.ads, &self.defaults, request, |_| {})
            }
        }
    }

    /// Resolve an HTTP method, URL, and headers into a set of [Endpoint]s.
    ///
    /// When multiple endpoints are returned, a client should send traffic to
    /// ALL of the returned endpoints because the routing policy specified
    /// that traffic should be mirrored.
    pub fn resolve_http(
        &mut self,
        method: &http::Method,
        url: &crate::Url,
        headers: &http::HeaderMap,
    ) -> crate::Result<Vec<crate::Endpoint>> {
        let request = HttpRequest {
            method,
            url,
            headers,
        };

        // FIXME: this is deeply janky, manual exponential backoff. it should be
        // possible for the ADS client to signal when a cache entry is
        // available/pending a request/not found instead of just there/not.
        //
        // make that happen, and figure out if there is a way to notify when
        // that happens.
        const RESOLVE_BACKOFF: &[Duration] = &[
            Duration::from_millis(1),
            Duration::from_millis(4),
            Duration::from_millis(16),
            Duration::from_millis(64),
            Duration::from_millis(256),
        ];

        for backoff in RESOLVE_BACKOFF {
            match self.resolve(request) {
                Ok(endpoints) => return Ok(endpoints),
                Err(e) => {
                    if !e.is_temporary() {
                        return Err(e);
                    }
                    std::thread::sleep(*backoff);
                }
            }
        }
        self.resolve(request)
    }

    fn resolve(&self, request: HttpRequest<'_>) -> crate::Result<Vec<crate::Endpoint>> {
        let resolved_route = self.resolve_routes(ConfigMode::Dynamic, request)?;

        self.ads
            .subscribe_to_backends(Some(resolved_route.backend.clone()))
            .unwrap();

        resolve_endpoint(&self.ads, &self.defaults, resolved_route, request)
    }
}

fn is_generated_route(route: &Route) -> bool {
    route
        .tags
        .contains_key(junction_api::http::tags::GENERATED_BY)
}

/// Generate the list of Targets that this URL maps to, taking into account the
/// URL's `port` and any search path rules. The list of targets will be returned
/// in the most-to-least specific order.
///
/// The URL's explicitly listed port or the default port for the URL's scheme
/// will also be used to first specify a port-specific target before falling
/// back to a port-less target.
pub(crate) fn vhosts_for_url(url: &crate::Url) -> crate::Result<Vec<VirtualHost>> {
    let target =
        Target::from_str(url.hostname()).map_err(|e| Error::into_invalid_url(e.to_string()))?;

    Ok(vec![
        target.clone().into_vhost(Some(url.default_port())),
        target.into_vhost(None),
    ])
}

/// A view into an HTTP Request, before any rewrites or modifications have been
/// made while sending or processing a response.
#[derive(Debug, Clone, Copy)]
pub struct HttpRequest<'a> {
    /// The HTTP Method of the request.
    pub method: &'a http::Method,

    /// The request URL, before any rewrites or modifications have been made.
    pub url: &'a crate::Url,

    /// The request headers, before
    pub headers: &'a http::HeaderMap,
}

/// The result of [resolving a route][Client::resolve_routes].
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    /// The resolved route.
    pub route: Arc<Route>,

    /// The index of the rule that matched the request. This will be missing if
    /// the matched route was the empty route, which trivially matches all
    /// requests to its [VirtualHost].
    pub rule: Option<usize>,

    /// The backend selected as part of route resolution.
    pub backend: BackendId,
}

pub(crate) fn resolve_routes<F>(
    cache: &impl ConfigCache,
    defaults: &impl ConfigCache,
    request: HttpRequest<'_>,
    subscribe: F,
) -> crate::Result<ResolvedRoute>
where
    F: Fn(&[VirtualHost]),
{
    use rand::seq::SliceRandom;

    let vhosts = vhosts_for_url(request.url)?;
    subscribe(&vhosts);

    let default_route = defaults.get_route_with_fallbacks(&vhosts);
    let configured_route = cache.get_route_with_fallbacks(&vhosts);

    // FIXME: for now, the default routes are only looked up if there is no
    // route coming from XDS. Whereas in reality we likely want to merge the
    // values. However that requires some thinking about what it means at
    // the rule equivalence level and is so left for later.
    let matching_route = match (default_route, configured_route) {
        (Some(default_route), Some(configured_route)) => {
            if is_generated_route(&configured_route) {
                default_route.clone()
            } else {
                configured_route
            }
        }
        (None, Some(configured_route)) => configured_route,
        (Some(default_route), None) => default_route.clone(),
        _ => return Err(Error::no_route_matched(vhosts)),
    };

    // if this is the trivial route, match it immediately and return
    if matching_route.rules.is_empty() {
        let backend = matching_route
            .vhost
            .with_default_port(request.url.default_port())
            .into_backend()
            .unwrap();

        return Ok(ResolvedRoute {
            route: matching_route,
            rule: None,
            backend,
        });
    }

    // if we got here, we have resolved to a list of routes
    let Some((matching_rule_idx, matching_rule)) = find_matching_rule(
        &matching_route,
        request.method,
        request.url,
        request.headers,
    ) else {
        return Err(Error::no_rule_matched(matching_route.vhost.clone()));
    };

    // pick a target at random from the list, respecting weights.
    //
    // if the list of backends is empty, allow falling through to the Route's
    // vhost by using either the vhost port or the request port.
    let weighted_backend = &crate::rand::with_thread_rng(|rng| {
        matching_rule.backends.choose_weighted(rng, |wc| wc.weight)
    });
    let backend = match weighted_backend {
        Ok(wb) => wb.backend.clone(),
        Err(WeightedError::NoItem) => matching_route
            .vhost
            .with_default_port(request.url.default_port())
            .into_backend()
            .unwrap(),
        Err(_) => {
            return Err(Error::invalid_route(
                "backends weights are invalid: total weights must be greater than zero",
                matching_route.vhost.clone(),
                matching_rule_idx,
            ))
        }
    };
    Ok(ResolvedRoute {
        route: matching_route.clone(),
        rule: Some(matching_rule_idx),
        backend,
    })
}

fn resolve_endpoint(
    cache: &impl ConfigCache,
    defaults: &impl ConfigCache,
    resolved: ResolvedRoute,
    request: HttpRequest<'_>,
) -> crate::Result<Vec<Endpoint>> {
    // pull the backend out of cache.
    let backend = match (
        cache.get_backend(&resolved.backend),
        defaults.get_backend(&resolved.backend),
    ) {
        // if there's a backend in both the cache and defaults, pick the cached
        // backend unless it's unspecified
        (Some(cache), Some(default)) if cache.config.lb.is_unspecified() => default,
        // otherwise pick the cached backend if it's available and the default
        // if it's not.
        (Some(backend), _) | (None, Some(backend)) => backend,
        // if there's nothign available, that's a paddlin
        (None, None) => {
            return Err(Error::no_backend(
                resolved.route.vhost.clone(),
                resolved.rule,
                resolved.backend,
            ))
        }
    };

    // there's no notion of defaults for endpoints (yet?).
    let Some(endpoints) = cache.get_endpoints(&resolved.backend) else {
        return Err(Error::no_reachable_endpoints(
            resolved.route.vhost.clone(),
            backend.config.id.clone(),
        ));
    };

    let endpoint = backend
        .load_balancer
        .load_balance(request.url, request.headers, &endpoints);
    let Some(endpoint) = endpoint else {
        return Err(Error::no_reachable_endpoints(
            resolved.route.vhost.clone(),
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

    Ok(vec![crate::Endpoint {
        url,
        timeouts,
        retry,
        address: endpoint.clone(),
    }])
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
    use junction_api::{http::WeightedBackend, Regex};
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_resolve_passthrough_route() {
        let target = Target::dns("example.com").unwrap();
        let routes = StaticConfig::new(
            vec![Route::passthrough_route(target.clone().into_vhost(None))],
            vec![],
        );
        let defaults = StaticConfig::default();

        let request = HttpRequest {
            method: &http::Method::GET,
            url: &Url::from_str("http://example.com/test-path").unwrap(),
            headers: &http::HeaderMap::default(),
        };

        let resolved = resolve_routes(&routes, &defaults, request, |_| {}).unwrap();
        assert_eq!(resolved.backend, target.into_backend(80));
    }

    #[test]
    fn test_resolve_route_no_rules() {
        let route = Route {
            vhost: Target::dns("example.com").unwrap().into_vhost(None),
            tags: Default::default(),
            rules: vec![],
        };

        let routes = StaticConfig::new(vec![route], vec![]);
        let defaults = StaticConfig::default();

        let resolved = resolve_routes(
            &routes,
            &defaults,
            HttpRequest {
                method: &http::Method::GET,
                url: &Url::from_str("http://example.com:3214/users/123").unwrap(),
                headers: &http::HeaderMap::default(),
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(resolved.rule, None);
        assert_eq!(
            resolved.backend,
            Target::dns("example.com").unwrap().into_backend(3214)
        )
    }

    #[test]
    fn test_resolve_route_no_backends() {
        let route = Route {
            vhost: Target::dns("example.com").unwrap().into_vhost(None),
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
        let defaults = StaticConfig::default();

        for port in [80, 7887] {
            let resolved = resolve_routes(
                &routes,
                &defaults,
                HttpRequest {
                    method: &http::Method::GET,
                    url: &Url::from_str(&format!("http://example.com:{port}/users/123")).unwrap(),
                    headers: &http::HeaderMap::default(),
                },
                |_| {},
            )
            .unwrap();

            assert_eq!(
                resolved.backend,
                Target::dns("example.com").unwrap().into_backend(port)
            )
        }
    }

    #[test]
    fn test_resolve_path_route() {
        let backend_one = Target::kube_service("web", "svc1")
            .unwrap()
            .into_backend(8910);
        let backend_two = Target::kube_service("web", "svc2")
            .unwrap()
            .into_backend(8919);

        let route = Route {
            vhost: Target::dns("example.com").unwrap().into_vhost(None),
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/users".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![WeightedBackend {
                        weight: 1,
                        backend: backend_one.clone(),
                    }],
                    ..Default::default()
                },
                RouteRule {
                    backends: vec![WeightedBackend {
                        weight: 1,
                        backend: backend_two.clone(),
                    }],
                    ..Default::default()
                },
            ],
        };

        let routes = StaticConfig::new(vec![route], vec![]);
        let defaults = StaticConfig::default();

        let resolved = resolve_routes(
            &routes,
            &defaults,
            HttpRequest {
                method: &http::Method::GET,
                url: &Url::from_str("http://example.com/test-path").unwrap(),
                headers: &http::HeaderMap::default(),
            },
            |_| {},
        )
        .unwrap();
        // should match the fallthrough rule
        assert_eq!(resolved.rule, Some(1));
        assert_eq!(resolved.backend, backend_two);

        let resolved = resolve_routes(
            &routes,
            &defaults,
            HttpRequest {
                method: &http::Method::GET,
                url: &Url::from_str("http://example.com/users/123").unwrap(),
                headers: &http::HeaderMap::default(),
            },
            |_| {},
        )
        .unwrap();
        // should match the first rule, with the path match
        assert_eq!(resolved.backend, backend_one);
        assert!(!resolved.route.rules[resolved.rule.unwrap()]
            .matches
            .is_empty());

        let resolved = resolve_routes(
            &routes,
            &defaults,
            HttpRequest {
                method: &http::Method::GET,
                url: &Url::from_str("http://example.com/users/123").unwrap(),
                headers: &http::HeaderMap::default(),
            },
            |_| {},
        )
        .unwrap();
        // should match the first rule, with the path match
        assert_eq!(resolved.rule, Some(0));
        assert_eq!(resolved.backend, backend_one);
    }

    #[test]
    fn test_resolve_query_route() {
        let backend_one = Target::kube_service("web", "svc1")
            .unwrap()
            .into_backend(8910);
        let backend_two = Target::kube_service("web", "svc2")
            .unwrap()
            .into_backend(8919);

        let route = Route {
            vhost: Target::dns("example.com").unwrap().into_vhost(None),
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
                    backends: vec![WeightedBackend {
                        weight: 1,
                        backend: backend_one.clone(),
                    }],
                    ..Default::default()
                },
                RouteRule {
                    backends: vec![WeightedBackend {
                        weight: 1,
                        backend: backend_two.clone(),
                    }],
                    ..Default::default()
                },
            ],
        };

        let routes = StaticConfig::new(vec![route], vec![]);
        let defaults = StaticConfig::default();

        let wont_match = [
            "http://example.com?qp1=tomato",
            "http://example.com?qp1=potatooo",
            "http://example.com?qp2=barfoo",
            "http://example.com?qp2=fobar",
            "http://example.com?qp1=potat&qp2=foobar",
            "http://example.com?qp1=potato&qp2=fbar",
        ];

        for url in wont_match {
            let resolved = resolve_routes(
                &routes,
                &defaults,
                HttpRequest {
                    method: &http::Method::GET,
                    url: &Url::from_str(url).unwrap(),
                    headers: &http::HeaderMap::default(),
                },
                |_| {},
            )
            .unwrap();
            // should match the fallthrough rule
            assert_eq!(resolved.rule, Some(1));
            assert_eq!(resolved.backend, backend_two);
        }

        let will_match = [
            "http://example.com?qp1=potato&qp2=foobar",
            "http://example.com?qp1=potato&qp2=foobazbar",
            "http://example.com?qp1=potato&qp2=fooooooooooooooobar",
        ];

        for url in will_match {
            let resolved = resolve_routes(
                &routes,
                &defaults,
                HttpRequest {
                    method: &http::Method::GET,
                    url: &Url::from_str(url).unwrap(),
                    headers: &http::HeaderMap::default(),
                },
                |_| {},
            )
            .unwrap();
            // should match one of the query matches
            assert_eq!(
                (resolved.rule, &resolved.backend),
                (Some(0), &backend_one),
                "should match the first rule: {url}"
            );
        }
    }
}
