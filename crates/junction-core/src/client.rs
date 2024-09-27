use crate::{
    config::{BackendLb, LoadBalancer},
    xds::{self, AdsClient},
};
use junction_api_types::{
    backend::Backend,
    http::{HeaderMatch, PathMatch, QueryParamMatch, Route, RouteMatch, RouteRule},
    shared::Attachment,
};
use std::time::Duration;
use std::{collections::BTreeSet, sync::Arc};
use std::{collections::HashMap, future::Future};

/// A service discovery client that looks up URL information based on URLs,
/// headers, and methods.
///
/// Clients use a shared in-memory cache to keep data warm so that a request
/// never has to block on a remote service.
///
/// Clients are cheaply cloneable, and should be cloned to create multiple
/// clients that share the same in-memory cache. Note clones do not compy across
/// any defaults
pub struct Client {
    ads: AdsClient,
    _ads_task: Arc<tokio::task::JoinHandle<()>>,
    // the default routes are keyed off the route's XDS listener name
    default_routes: HashMap<String, Arc<Route>>,
    // the default backends are keyed off the backends's XDS cluster name
    default_backends: HashMap<String, Arc<BackendLb>>,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            ads: self.ads.clone(),
            _ads_task: self._ads_task.clone(),
            default_routes: HashMap::default(),
            default_backends: HashMap::default(),
        }
    }
}

fn validate_defaults(_: &[Route], _: &[Backend]) -> Result<(), crate::Error> {
    Ok(())
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
            default_routes: HashMap::default(),
            default_backends: HashMap::default(),
            ads,
            _ads_task: Arc::new(handle),
        };

        Ok(client)
    }

    pub fn with_defaults(
        self,
        default_routes: Vec<Route>,
        default_backends: Vec<Backend>,
    ) -> Result<Client, crate::Error> {
        validate_defaults(&default_routes, &default_backends)?;

        let default_routes = default_routes
            .into_iter()
            .map(|x| (x.attachment.as_listener_xds_name(), Arc::new(x)))
            .collect();

        let default_backends = default_backends
            .into_iter()
            .map(|config| {
                let load_balancer = LoadBalancer::from_config(&config.lb);
                (
                    config.attachment.as_cluster_xds_name(),
                    Arc::new(BackendLb {
                        config,
                        load_balancer,
                    }),
                )
            })
            .collect();

        self.subscribe_to_defaults(&default_routes);

        Ok(Client {
            default_routes,
            default_backends,
            ..self
        })
    }

    fn subscribe_to_defaults(&self, default_routes: &HashMap<String, Arc<Route>>) {
        for (route_name, route) in default_routes {
            self.ads
                .subscribe(xds::ResourceType::Listener, route_name.to_string())
                .unwrap();

            for rule in &route.rules {
                for backend in &rule.backends {
                    self.ads
                        .subscribe(
                            xds::ResourceType::Cluster,
                            backend.attachment.as_cluster_xds_name(),
                        )
                        .unwrap();
                }
            }
        }
    }

    /// Start a gRPC CSDS server on the given port.
    ///
    /// To run the server, you must `await` this future.
    pub fn csds_server(
        &self,
        port: u16,
    ) -> impl Future<Output = Result<(), tonic::transport::Error>> {
        crate::xds::csds::local_server(self.ads.cache.clone(), port)
    }

    /// Dump the client's current cache of xDS resources, as fetched from the
    /// config server.
    ///
    /// This is a programmatic view of the same data that you can fetch over
    /// gRPC by starting a [Client::csds_server].
    pub fn dump_xds(&self) -> impl Iterator<Item = crate::XdsConfig> + '_ {
        self.ads.cache.iter_xds()
    }

    /// Dump the Client's current table of [Route]s, merging together any
    /// default routes and remotely fetched routes the same way the client would
    /// when resolving endpoints.
    pub fn dump_routes(&self) -> Vec<Arc<Route>> {
        let mut routes = vec![];
        let mut defaults: BTreeSet<_> = self.default_routes.keys().collect();

        for route in self.ads.cache.iter_routes() {
            let route = if route.is_default_route() {
                self.default_routes
                    .get(&route.attachment.as_listener_xds_name())
                    .cloned()
                    .unwrap_or(route)
            } else {
                route
            };
            defaults.remove(&route.attachment.as_listener_xds_name());
            routes.push(route);
        }

        for route_name in defaults {
            // safety: this started as the key set of default_routes
            routes.push(self.default_routes.get(route_name).unwrap().clone());
        }

        routes
    }

    /// Dump the Client's current table of [BackendLb]s, merging together any
    /// default configuration and remotely fetched config the same way the
    /// client would when resolving endpoints.
    pub fn dump_backends(&self) -> Vec<Arc<BackendLb>> {
        let mut backends = vec![];
        let mut defaults: BTreeSet<_> = self.default_backends.keys().collect();

        for backend in self.ads.cache.iter_backends() {
            let backend = if backend.config.lb.is_default_policy() {
                self.default_backends
                    .get(&backend.config.attachment.as_cluster_xds_name())
                    .cloned()
                    .unwrap_or(backend)
            } else {
                backend
            };

            defaults.remove(&backend.config.attachment.as_cluster_xds_name());
            backends.push(backend);
        }

        for backend_name in defaults {
            backends.push(self.default_backends.get(backend_name).unwrap().clone());
        }

        backends
    }

    pub fn resolve_endpoints(
        &mut self,
        method: &http::Method,
        uri: http::Uri,
        headers: &http::HeaderMap,
    ) -> crate::Result<Vec<crate::Endpoint>> {
        let mut url = crate::Url::new(uri)?;

        // TODO: there's really no reasonable way to recover without starting a
        // new client. if you're on the default client (like FFI clients
        // probably are?) can you do anything about this?
        //
        // TODO: increment a metric or something if there's an error. this
        // shouldn't be a showstopper if data is already in cache.
        let _ = self
            .ads
            .subscribe(xds::ResourceType::Listener, url.hostname().to_string());

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
            match self.get_endpoints(method, url, headers) {
                Ok(endpoints) => return Ok(endpoints),
                Err((u, e)) => {
                    if !e.is_temporary() {
                        return Err(e);
                    }

                    url = u;
                    std::thread::sleep(*backoff);
                }
            }
        }

        self.get_endpoints(method, url, headers)
            .map_err(|(_url, e)| e)
    }
}

impl Client {
    fn get_endpoints(
        &self,
        method: &http::Method,
        url: crate::Url,
        headers: &http::HeaderMap,
    ) -> Result<Vec<crate::Endpoint>, (crate::Url, crate::Error)> {
        use rand::seq::SliceRandom;

        let key_with_port = Attachment::from_hostname(url.hostname(), Some(url.port()));
        let key_no_port = Attachment::from_hostname(url.hostname(), None);

        // FIXME: for now, the default routes are only looked up if there is no
        // route coming from XDS. Whereas in reality we likely want to merge the
        // values. However that requires some thinking about what it means at
        // the rule equivalence level and is so left for later.
        let default_route = self
            .default_routes
            .get(&key_with_port.as_listener_xds_name())
            .or_else(|| self.default_routes.get(&key_no_port.as_listener_xds_name()));
        let configured_route = self
            .ads
            .get_route(&key_with_port)
            .or_else(|| self.ads.get_route(&key_no_port));

        let matching_route = match (default_route, configured_route) {
            (Some(default_route), Some(configured_route)) => {
                if configured_route.is_default_route() {
                    default_route.clone()
                } else {
                    configured_route
                }
            }
            (None, Some(configured_route)) => configured_route,
            (Some(default_route), None) => default_route.clone(),
            _ => {
                return Err((
                    url,
                    crate::Error::NoRouteMatched {
                        routes: vec![key_with_port, key_no_port],
                    },
                ))
            }
        };

        // if we got here, we have resolved to a list of routes
        let Some((matching_rule_idx, matching_rule)) =
            find_matching_rule(&matching_route, method, &url, headers)
        else {
            return Err((
                url,
                crate::Error::NoRuleMatched {
                    route: matching_route.attachment.clone(),
                },
            ));
        };

        // pick a target at random from the list, respecting weights. if the
        // target has no port, then then we need to fill in the default using
        // the request URL.
        //
        // FIXME: this ignores the request URL if the backend has a port? is
        // that right?
        let backend_id = &crate::rand::with_thread_rng(|rng| {
            matching_rule.backends.choose_weighted(rng, |wc| wc.weight)
        });
        let Ok(backend_id) = backend_id.map(|w| &w.attachment) else {
            return Err((
                url,
                crate::Error::InvalidRoutes {
                    message: "matched rule has no backends",
                    attachment: matching_route.attachment.clone(),
                    rule: matching_rule_idx,
                },
            ));
        };
        let backend_id = match backend_id.port() {
            Some(_) => backend_id.clone(),
            None => backend_id.with_port(url.port()),
        };

        // use the load
        let (backend, endpoints) = match self.ads.get_target(&backend_id) {
            (Some(backend), Some(endpoints)) => {
                let lb = if backend.config.lb.is_default_policy() {
                    self.default_backends
                        .get(&backend_id.as_cluster_xds_name())
                        .cloned()
                        .unwrap_or(backend)
                } else {
                    backend
                };
                (lb, endpoints)
            }
            (None, Some(endpoints)) => {
                let Some(lb) = self.default_backends.get(&backend_id.as_cluster_xds_name()) else {
                    return Err((
                        url,
                        crate::Error::NoBackend {
                            route: matching_route.attachment.clone(),
                            rule: matching_rule_idx,
                            backend: backend_id,
                        },
                    ));
                };
                (lb.clone(), endpoints)
            }
            _ => {
                // FIXME(DNS): this might be something we want to handle
                // depending on exactly where DNS lookups get implemented. we
                // still need to check client defaults as its entirly possible
                // its a DNS address that xDS knows nothing about but we can
                // still route to.
                return Err((
                    url,
                    crate::Error::NoBackend {
                        route: matching_route.attachment.clone(),
                        rule: matching_rule_idx,
                        backend: backend_id,
                    },
                ));
            }
        };

        let endpoint = match backend.load_balancer.load_balance(
            &url,
            headers,
            &matching_rule.session_affinity,
            &endpoints,
        ) {
            Some(e) => e,
            None => {
                return Err((
                    url,
                    crate::Error::NoReachableEndpoints {
                        route: matching_route.attachment.clone(),
                        backend: backend_id,
                    },
                ))
            }
        };
        let timeouts = matching_rule.timeouts.clone();
        let retry = matching_rule.retry_policy.clone();

        Ok(vec![crate::Endpoint {
            url,
            timeouts,
            retry,
            address: endpoint.clone(),
        }])
    }
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
    let Some(header_val) = headers.get(&rule.name) else {
        return false;
    };
    let Ok(header_val) = header_val.to_str() else {
        return false;
    };
    rule.value_matcher.is_match(header_val)
}

pub fn is_query_params_match(rule: &QueryParamMatch, query: Option<&str>) -> bool {
    let Some(query) = query else {
        return false;
    };
    for (param, value) in form_urlencoded::parse(query.as_bytes()) {
        if param == rule.name {
            return rule.value_matcher.is_match(&value);
        }
    }
    false
}
