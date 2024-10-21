//! An XDS-compatible HTTP Client.

mod error;
mod url;
pub use crate::error::{Error, Result};
pub use crate::url::Url;

pub(crate) mod rand;

mod endpoints;
pub use endpoints::{Endpoint, EndpointAddress};

mod client;
mod load_balancer;
mod xds;

pub use client::Client;
pub use xds::{ResourceVersion, XdsConfig};

use junction_api::http::Route;
use junction_api::Target;
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::load_balancer::BackendLb;
use crate::load_balancer::EndpointGroup;
pub use crate::load_balancer::LoadBalancer;
use junction_api::backend::Backend;

/// Check route resolution.
///
/// Resolves a route against a table of routes, returning the chosen [Route],
/// the index of the rule that matched, and the [Target] selected from the
/// route.
///
/// [Client::resolve_http] resolves routes in exactly the same way as this
/// function. Use it to test routing configuration without requiring a full
/// client or connecting to a control plane.
pub fn check_route(
    routes: Vec<Route>,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Result<(Route, usize, Target)> {
    let config = StaticConfig::new(routes, Vec::new());

    let request = client::HttpRequest {
        method,
        url,
        headers,
    };

    let targets = client::targets_for_url(url)?;
    let resolved = client::resolve_routes(&StaticConfig::default(), &config, request, targets)?;

    // Drop the other copies of the resolved routes and unwrap the Arc.
    //
    // safety: we completely own these routes, and the only copies should be
    // the one returned and the one in `config` we're dropping here.
    std::mem::drop(config);
    let route = Arc::into_inner(resolved.route).unwrap();
    Ok((route, resolved.rule, resolved.backend))
}

pub(crate) trait ConfigCache {
    fn get_route(&self, target: &Target) -> Option<Arc<Route>>;
    fn get_backend(&self, target: &Target) -> (Option<Arc<BackendLb>>, Option<Arc<EndpointGroup>>);

    fn get_route_with_fallbacks(&self, targets: &[Target]) -> Option<Arc<Route>> {
        for target in targets {
            if let Some(route) = self.get_route(target) {
                return Some(route);
            }
        }

        None
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StaticConfig {
    pub routes: HashMap<Target, Arc<Route>>,
    pub backends: HashMap<Target, Arc<BackendLb>>,
}

impl StaticConfig {
    pub(crate) fn new(routes: Vec<Route>, backends: Vec<Backend>) -> Self {
        let routes = routes
            .into_iter()
            .map(|x| (x.target.clone(), Arc::new(x)))
            .collect();

        let backends = backends
            .into_iter()
            .map(|config| {
                let load_balancer = LoadBalancer::from_config(&config.lb);
                (
                    config.target.clone(),
                    Arc::new(BackendLb {
                        config,
                        load_balancer,
                    }),
                )
            })
            .collect();

        Self { routes, backends }
    }
}

impl ConfigCache for StaticConfig {
    fn get_route(&self, target: &Target) -> Option<Arc<Route>> {
        self.routes.get(target).cloned()
    }

    fn get_backend(&self, target: &Target) -> (Option<Arc<BackendLb>>, Option<Arc<EndpointGroup>>) {
        (self.backends.get(target).cloned(), None)
    }
}
