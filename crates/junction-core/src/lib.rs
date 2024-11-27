//! An XDS-compatible HTTP Client.

mod error;
mod url;
pub use crate::error::{Error, Result};
pub use crate::url::Url;

pub(crate) mod rand;

mod endpoints;
pub use endpoints::{Endpoint, EndpointAddress};

mod client;
mod dns;
mod load_balancer;
mod xds;

pub use client::{Client, HttpRequest, ResolveMode, ResolvedRoute};
use futures::FutureExt;
pub use xds::{ResourceVersion, XdsConfig};

use junction_api::http::Route;
use junction_api::{BackendId, VirtualHost};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

pub use crate::load_balancer::BackendLb;
use crate::load_balancer::EndpointGroup;
pub use crate::load_balancer::LoadBalancer;
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
) -> Result<ResolvedRoute> {
    let request = client::HttpRequest::from_parts(method, url, headers)?;
    // resolve with an empty cache and the passed config used as defaults and a
    // no-op subscribe fn.
    //
    // TODO: do we actually want that or do we want to treat the passed routes
    // as the primary config?
    let config = StaticConfig::new(routes, Vec::new());

    // resolve_routes is async but we know that with StaticConfig, fetching
    // config should NEVER block. now-or-never just calls Poll with a noop
    // waker and unwraps the result ASAP.
    client::resolve_routes(&config, request)
        .now_or_never()
        .expect("check_route yielded unexpectedly. this is a bug in Junction, please file an issue")
}

pub(crate) trait ConfigCache {
    async fn get_route(&self, target: &VirtualHost) -> Option<Arc<Route>>;
    async fn get_backend(&self, target: &BackendId) -> Option<Arc<BackendLb>>;
    async fn get_endpoints(&self, backend: &BackendId) -> Option<Arc<EndpointGroup>>;
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StaticConfig {
    pub routes: HashMap<VirtualHost, Arc<Route>>,
    pub backends: HashMap<BackendId, Arc<BackendLb>>,
}

impl StaticConfig {
    pub(crate) fn new(routes: Vec<Route>, backends: Vec<Backend>) -> Self {
        let routes: HashMap<_, _> = routes
            .into_iter()
            .map(|x| (x.vhost.clone(), Arc::new(x)))
            .collect();

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
        let mut routes: HashMap<_, _> = routes
            .into_iter()
            .map(|x| (x.vhost.clone(), Arc::new(x)))
            .collect();

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

        // infer default backends
        let mut inferred_backends = vec![];
        for route in routes.values() {
            for rule in &route.rules {
                for wb in &rule.backends {
                    if !backends.contains_key(&wb.backend) {
                        let id = wb.backend.clone();
                        let config = Backend {
                            id: id.clone(),
                            lb: LbPolicy::default(),
                        };
                        let load_balancer = LoadBalancer::from_config(&config.lb);
                        inferred_backends.push((
                            id,
                            Arc::new(BackendLb {
                                config,
                                load_balancer,
                            }),
                        ));
                    }
                }
            }
        }

        // infer default routes
        let mut inferred_routes = vec![];
        for backend in backends.values() {
            let vhost = backend.config.id.clone().into_vhost().without_port();
            if !routes.contains_key(&vhost) {
                inferred_routes.push((vhost.clone(), Arc::new(Route::passthrough_route(vhost))));
            }
        }

        routes.extend(inferred_routes);
        backends.extend(inferred_backends);

        Self { routes, backends }
    }

    pub(crate) fn backends(&self) -> Vec<BackendId> {
        let mut backends = Vec::with_capacity(self.routes.len() + self.backends.len());

        // backends
        backends.extend(self.backends.keys().cloned());

        // all of the route targets
        for route in self.routes.values() {
            for rule in &route.rules {
                for wb in &rule.backends {
                    backends.push(wb.backend.clone());
                }
            }
        }

        backends.sort();
        backends.dedup();

        backends
    }
}

impl ConfigCache for StaticConfig {
    fn get_route(&self, target: &VirtualHost) -> impl Future<Output = Option<Arc<Route>>> {
        std::future::ready(self.routes.get(target).cloned())
    }

    fn get_backend(&self, target: &BackendId) -> impl Future<Output = Option<Arc<BackendLb>>> {
        std::future::ready(self.backends.get(target).cloned())
    }

    fn get_endpoints(&self, _: &BackendId) -> impl Future<Output = Option<Arc<EndpointGroup>>> {
        std::future::ready(None)
    }
}
