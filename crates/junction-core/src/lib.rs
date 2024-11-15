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

pub use client::{Client, ConfigMode, HttpRequest, ResolvedRoute};
pub use xds::{ResourceVersion, XdsConfig};

use junction_api::http::Route;
use junction_api::{BackendId, VirtualHost};
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::load_balancer::BackendLb;
use crate::load_balancer::EndpointGroup;
pub use crate::load_balancer::LoadBalancer;
use junction_api::backend::Backend;

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
    let request = client::HttpRequest {
        method,
        url,
        headers,
    };

    // resolve with an empty cache and the passed config used as defaults and a
    // no-op subscribe fn.
    //
    // TODO: do we actually want that or do we want to treat the passed routes
    // as the primary config?
    let config = StaticConfig::new(routes, Vec::new());
    client::resolve_routes(&StaticConfig::default(), &config, request, |_| {})
}

pub(crate) trait ConfigCache {
    fn get_route(&self, target: &VirtualHost) -> Option<Arc<Route>>;
    fn get_backend(&self, target: &BackendId) -> Option<Arc<BackendLb>>;
    fn get_endpoints(&self, backend: &BackendId) -> Option<Arc<EndpointGroup>>;

    fn get_route_with_fallbacks(&self, targets: &[VirtualHost]) -> Option<Arc<Route>> {
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
    pub routes: HashMap<VirtualHost, Arc<Route>>,
    pub backends: HashMap<BackendId, Arc<BackendLb>>,
}

impl StaticConfig {
    pub(crate) fn new(routes: Vec<Route>, backends: Vec<Backend>) -> Self {
        let routes = routes
            .into_iter()
            .map(|x| (x.vhost.clone(), Arc::new(x)))
            .collect();

        let backends = backends
            .into_iter()
            .map(|config| {
                let load_balancer = LoadBalancer::from_config(&config.lb);
                (
                    config.id.clone(),
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
    fn get_route(&self, target: &VirtualHost) -> Option<Arc<Route>> {
        self.routes.get(target).cloned()
    }

    fn get_backend(&self, target: &BackendId) -> Option<Arc<BackendLb>> {
        self.backends.get(target).cloned()
    }

    fn get_endpoints(&self, _: &BackendId) -> Option<Arc<EndpointGroup>> {
        None
    }
}
