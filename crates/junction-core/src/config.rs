use std::collections::HashMap;
use std::sync::Arc;

use junction_api::backend::Backend;

pub(crate) mod endpoints;
pub(crate) mod load_balancer;
use junction_api::http::Route;
use junction_api::shared::Target;
pub(crate) use load_balancer::EndpointGroup;
pub(crate) use load_balancer::LoadBalancer;

/// A [Backend] and the [LoadBalancer] it's configured with.
#[derive(Debug)]
pub struct BackendLb {
    pub config: Backend,
    pub load_balancer: LoadBalancer,
}

pub(crate) trait ConfigCache {
    fn get_route(&self, target: &Target) -> Option<Arc<Route>>;
    fn get_backend(&self, target: &Target) -> (Option<Arc<BackendLb>>, Option<Arc<EndpointGroup>>);
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StaticConfig {
    pub routes: HashMap<String, Arc<Route>>,
    pub backends: HashMap<String, Arc<BackendLb>>,
}

impl StaticConfig {
    pub(crate) fn new(routes: Vec<Route>, backends: Vec<Backend>) -> Self {
        let routes = routes
            .into_iter()
            .map(|x| (x.target.as_listener_xds_name(), Arc::new(x)))
            .collect();

        let backends = backends
            .into_iter()
            .map(|config| {
                let load_balancer = LoadBalancer::from_config(&config.lb);
                (
                    config.target.as_cluster_xds_name(),
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
        self.routes.get(&target.as_listener_xds_name()).cloned()
    }

    fn get_backend(&self, target: &Target) -> (Option<Arc<BackendLb>>, Option<Arc<EndpointGroup>>) {
        (
            self.backends.get(&target.as_cluster_xds_name()).cloned(),
            None,
        )
    }
}
