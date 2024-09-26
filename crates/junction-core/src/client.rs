use crate::xds::{self, AdsClient};
use junction_api_types::{
    backend::Backend,
    http::{HeaderMatch, PathMatch, QueryParamMatch, Route, RouteMatch, RouteRule},
    shared::Attachment,
};
use std::sync::Arc;
use std::time::Duration;
use std::{collections::HashMap, future::Future};

struct DefaultCluster {
    // the arc here is completely unnecessary in true utility
    // but makes the match code much simpler
    pub load_balancer: Arc<crate::config::LoadBalancer>,
}

impl DefaultCluster {
    fn new(config: Backend) -> Self {
        let load_balancer = Arc::new(crate::config::LoadBalancer::from_config(&config.lb));
        Self { load_balancer }
    }
}

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
    default_routes: HashMap<String, Route>,
    // the default backends are keyed off the backends's XDS cluster name
    default_backends: HashMap<String, DefaultCluster>,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        return Self {
            ads: self.ads.clone(),
            _ads_task: self._ads_task.clone(),
            default_routes: HashMap::default(),
            default_backends: HashMap::default(),
        };
    }
}

impl Client {
    /// Build a new client, spawning a new ADS client in the background.
    ///
    /// This method creates a new ADS client and ADS connection. Data fetched
    /// over ADS won't be shared with existing clients. To create a client that
    /// shares with an existing cache, call [Client::clone] on an existing
    /// client.
    ///
    /// This function assumes that you're currently running the context of
    /// a `tokio` runtime and spawns background tasks.
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
    ) -> Client {
        //fixme: should validate defaults here

        let default_routes = default_routes
            .into_iter()
            .map(|x| (x.attachment.as_listener_xds_name(), x))
            .collect();

        let default_backends = default_backends
            .into_iter()
            .map(|x| (x.attachment.as_cluster_xds_name(), DefaultCluster::new(x)))
            .collect();

        self.subscribe_to_defaults();
        Client {
            default_routes,
            default_backends,
            ..self
        }
    }

    fn subscribe_to_defaults(&self) {
        for route in &self.default_routes {
            self.ads
                .subscribe(xds::ResourceType::Listener, route.0.to_string())
                .unwrap();

            for rule in &route.1.rules {
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

    pub fn config_server(
        &self,
        port: u16,
    ) -> impl Future<Output = Result<(), tonic::transport::Error>> {
        crate::xds::csds::local_server(self.ads.cache.clone(), port)
    }

    pub fn dump(&self) -> impl Iterator<Item = crate::XdsConfig> + '_ {
        self.ads.cache.iter_any()
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

        // FIXME: this is deeply janky, manual exponential backoff. it should
        // be possible for the ADS client to signal when a cache entry is
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

        // FIXME: for now, the default routes are only looked up if there is
        // no route coming from XDS. Whereas in reality we likely want to merge the
        // values. However that requires some thinking about what it means at the rule
        // equivalence level and is so left for later.
        let arc_holder;
        let matching_route_xds = self
            .ads
            .get_route(&key_with_port)
            .or_else(|| self.ads.get_route(&key_no_port));
        let matching_route = match matching_route_xds {
            Some(x) => {
                arc_holder = x;
                arc_holder.as_ref()
            }
            None => {
                let default = self
                    .default_routes
                    .get(&key_with_port.as_listener_xds_name())
                    .or_else(|| self.default_routes.get(&key_no_port.as_listener_xds_name()));

                if default.is_none() {
                    return Err((url, crate::Error::NoRouteMatched));
                }
                default.unwrap()
            }
        };

        //if we got here, we have resolved to a list of routes
        let Some(matching_rule) = find_matching_rule(matching_route, method, &url, headers) else {
            return Err((url, crate::Error::NoRuleMatched));
        };

        let backend = crate::rand::with_thread_rng(|rng| {
            matching_rule
                .backends
                .choose_weighted(rng, |wc| wc.weight)
                // TODO: this is really an invalid config error. we should have
                // caught this earlier. for now, panic here.
                .expect("tried to sample from an invalid config")
        });

        // if backend has no port, then then we need to fill in the default which
        // is the port of the request
        let backend_id = match backend.attachment.port() {
            Some(_) => backend.attachment.clone(),
            None => backend.attachment.with_port(url.port()),
        };

        let (lb, endpoints) = match self.ads.get_target(&backend_id) {
            (Some(lb), Some(endpoints)) => {
                // if the load balancer is simple, we allow a default
                // to overload
                // FIXME(ports): should we also allow a default without
                // a port set to overload
                match lb.as_ref() {
                    crate::config::LoadBalancer::Unspecified(_) => {
                        match self.default_backends.get(&backend_id.as_cluster_xds_name()) {
                            Some(x) => (x.load_balancer.clone(), endpoints),
                            None => (lb, endpoints),
                        }
                    }
                    _ => (lb, endpoints),
                }
            }
            (Some(_), None) => {
                // FIXME(DNS): this might be something we want to handle
                // depending on exactly where DNS lookups get implemented
                return Err((url, crate::Error::NoEndpoints));
            }
            (None, Some(endpoints)) => {
                match self.default_backends.get(&backend_id.as_cluster_xds_name()) {
                    Some(x) => (x.load_balancer.clone(), endpoints),
                    None => {
                        return Err((url, crate::Error::NoEndpoints));
                    }
                }
            }
            (None, None) => {
                // FIXME(DNS): in this case, we still need to check client defaults
                // as its entirly possible its a DNS address that xDS knows nothing about
                // but we can still route to.
                return Err((url, crate::Error::NoEndpoints));
            }
        };

        let endpoint =
            match lb.load_balance(&url, headers, &matching_rule.session_affinity, &endpoints) {
                Some(e) => e,
                None => return Err((url, crate::Error::NoReachableEndpoints)),
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

//FIXME(routing): picking between these is way more complicated than finding the first match
pub fn find_matching_rule<'a>(
    route: &'a Route,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Option<&'a RouteRule> {
    route
        .rules
        .iter()
        .find(|rule| is_route_rule_match(rule, method, url, headers))
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
