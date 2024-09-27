use junction_api_types::backend::Backend;

pub(crate) mod endpoints;
pub(crate) mod load_balancer;
pub(crate) use load_balancer::EndpointGroup;
pub(crate) use load_balancer::LoadBalancer;

/// A [Backend] and the [LoadBalancer] it's configured with.
#[derive(Debug)]
pub struct BackendLb {
    pub backend: Backend,
    pub load_balancer: LoadBalancer,
}
