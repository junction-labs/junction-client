//! The core implementation for Junction - an xDS dynamically-configurable API load-balancer library.
//!
//! * [Getting Started](https://docs.junctionlabs.io/getting-started/rust)

mod error;
mod url;
pub use crate::error::{Error, Result};
pub use crate::url::Url;

pub(crate) mod hash;
pub(crate) mod rand;

mod endpoints;
pub use endpoints::Endpoint;
use endpoints::EndpointGroup;

mod client;
mod dns;
mod load_balancer;
mod xds;

pub use client::{
    Client, HttpRequest, HttpResult, LbContext, ResolvedRoute, SearchConfig, SelectedEndpoint,
};
use error::Trace;
use futures::FutureExt;
use junction_api::Name;
pub use xds::{ResourceVersion, XdsConfig};

use junction_api::backend::BackendId;
use junction_api::http::Route;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

pub use crate::load_balancer::{BackendLb, LoadBalancer};
use junction_api::backend::{Backend, LbPolicy};

/// Check route resolution.
///
/// Resolves a route against a table of routes, returning the chosen [Route],
/// the index of the rule that matched, and the [BackendId] selected based on
/// the route.
///
/// Use this function to test routing configuration without requiring a full
/// client or a live connection to a control plane. For actual route resolution,
/// see [Client::resolve_http].
pub fn check_route(
    routes: Vec<Route>,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
    search_config: Option<&SearchConfig>,
) -> Result<ResolvedRoute> {
    let request = client::HttpRequest::from_parts(method, url, headers)?;
    // resolve with an empty cache and the passed config used as defaults and a
    // no-op subscribe fn.
    //
    // TODO: do we actually want that or do we want to treat the passed routes
    // as the primary config?
    let config = StaticConfig::new(routes, Vec::new());
    let search_config = search_config.cloned().unwrap_or_default();

    // resolve_routes is async but we know that with StaticConfig, fetching
    // config should NEVER block. now-or-never just calls Poll with a noop
    // waker and unwraps the result ASAP.
    client::resolve_routes(&config, Trace::new(), request, None, &search_config)
        .now_or_never()
        .expect("check_route yielded unexpectedly. this is a bug in Junction, please file an issue")
}

pub(crate) trait ConfigCache {
    async fn get_route<S: AsRef<str>>(&self, authority: S) -> Option<Arc<Route>>;
    async fn get_backend(&self, target: &BackendId) -> Option<Arc<BackendLb>>;
    async fn get_endpoints(&self, backend: &BackendId) -> Option<Arc<EndpointGroup>>;
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StaticConfig {
    pub routes: Vec<Arc<Route>>,
    pub backends: HashMap<BackendId, Arc<BackendLb>>,
}

impl StaticConfig {
    pub(crate) fn new(routes: Vec<Route>, backends: Vec<Backend>) -> Self {
        let routes = routes.into_iter().map(Arc::new).collect();

        let backends: HashMap<_, _> = backends
            .into_iter()
            .map(|config| {
                let load_balancer = LoadBalancer::from_config(&config.lb);
                let backend_id = config.id.clone();
                let backend_lb = Arc::new(BackendLb {
                    config,
                    load_balancer,
                });
                (backend_id, backend_lb)
            })
            .collect();

        Self { routes, backends }
    }

    pub(crate) fn with_inferred(routes: Vec<Route>, backends: Vec<Backend>) -> Self {
        let mut routes: Vec<_> = routes.into_iter().map(Arc::new).collect();
        let mut backends: HashMap<_, _> = backends
            .into_iter()
            .map(|config| {
                let load_balancer = LoadBalancer::from_config(&config.lb);
                let backend_id = config.id.clone();
                let backend_lb = Arc::new(BackendLb {
                    config,
                    load_balancer,
                });
                (backend_id, backend_lb)
            })
            .collect();

        // infer default backends for Routes with no specified backends.  we can
        // only infer a backend for a backendref with a port
        let mut inferred_backends = vec![];
        for route in &routes {
            for rule in &route.rules {
                for backend_ref in &rule.backends {
                    let Some(backend_id) = backend_ref.as_backend_id() else {
                        continue;
                    };

                    if backends.contains_key(&backend_id) {
                        continue;
                    }

                    let config = Backend {
                        id: backend_id.clone(),
                        lb: LbPolicy::default(),
                    };
                    let load_balancer = LoadBalancer::from_config(&config.lb);

                    inferred_backends.push((
                        backend_id,
                        Arc::new(BackendLb {
                            config,
                            load_balancer,
                        }),
                    ))
                }
            }
        }

        // infer default Routes for Backends. Track the set of Services
        // referenced all Routes, and create a new passthrough for every
        // Service that doesn't have one.
        let mut inferred_routes = vec![];
        let mut route_refs = HashSet::new();
        for route in &routes {
            for rule in &route.rules {
                for backend_ref in &rule.backends {
                    route_refs.insert(backend_ref.service.clone());
                }
            }
        }
        for backend in backends.values() {
            if !route_refs.contains(&backend.config.id.service) {
                let route = Route::passthrough_route(
                    Name::from_static("inferred"),
                    backend.config.id.service.clone(),
                );
                inferred_routes.push(Arc::new(route));
                route_refs.insert(backend.config.id.service.clone());
            }
        }

        routes.extend(inferred_routes);
        backends.extend(inferred_backends);

        Self { routes, backends }
    }
}

impl ConfigCache for StaticConfig {
    async fn get_route<S: AsRef<str>>(&self, authority: S) -> Option<Arc<Route>> {
        let (host, port) = authority.as_ref().split_once(":")?;
        let port = port.parse().ok()?;

        self.routes
            .iter()
            .find(|r| route_matches(r, host, port))
            .map(Arc::clone)
    }

    fn get_backend(&self, target: &BackendId) -> impl Future<Output = Option<Arc<BackendLb>>> {
        std::future::ready(self.backends.get(target).cloned())
    }

    fn get_endpoints(&self, _: &BackendId) -> impl Future<Output = Option<Arc<EndpointGroup>>> {
        std::future::ready(None)
    }
}

fn route_matches(route: &Route, host: &str, port: u16) -> bool {
    if !route.hostnames.iter().any(|h| h.matches_str(host)) {
        return false;
    }

    if !(route.ports.is_empty() || route.ports.contains(&port)) {
        return false;
    }

    true
}

#[cfg(test)]
mod test {
    use super::*;
    use junction_api::{
        http::{BackendRef, RouteRule},
        Hostname, Service,
    };
    use std::str::FromStr;

    #[test]
    fn test_check_routes_resolves_ndots_no_match() {
        let backend = Service::kube("web", "svc1").unwrap();

        let wont_match = ["http://not.example.com", "http://notexample.com"];

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

        let routes = vec![route];

        for url in wont_match {
            let url = crate::Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();

            let resolved_route = check_route(
                routes.clone(),
                &http::Method::GET,
                &url,
                headers,
                Some(&SearchConfig::new(3, vec![])),
            );

            match resolved_route {
                Ok(_) => panic!("succeeded for {} should have failed.", url.authority()),
                Err(e) => assert_eq!(
                    format!("{}", e),
                    format!("no route matched: '{}'", url.authority())
                ),
            }
        }
    }

    #[test]
    fn test_check_routes_resolves_ndots_match_without_search() {
        let backend = Service::kube("web", "svc1").unwrap();

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

        let routes = vec![route];

        let will_match = [
            "http://example.com",
            "http://example.foo.com",
            "http://example.foo.bar.com",
        ];

        for url in will_match {
            let url = crate::Url::from_str(url).unwrap();
            let headers = &http::HeaderMap::default();

            let resolved = check_route(
                routes.clone(),
                &http::Method::GET,
                &url,
                headers,
                Some(&SearchConfig::new(3, vec![])),
            )
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
    fn test_check_routes_resolves_ndots() {
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

        let routes = vec![route];

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

            let resolved = check_route(
                routes.clone(),
                &http::Method::GET,
                &url,
                headers,
                Some(&SearchConfig::new(3, will_match_hostnames.clone())),
            )
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
