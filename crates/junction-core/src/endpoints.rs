use junction_api::http::{RouteRetry, RouteTimeouts};
use std::{net::SocketAddr, sync::Arc};

use crate::load_balancer::EndpointGroup;

/// An HTTP endpoint to make a request to.
///
/// Endpoints contain both a target [url][crate::Url] that should be given to an
/// HTTP client and an [address][EndpointAddress] that indicates the address the
/// the hostname in the URL should resolve to. See [EndpointAddress] for more
/// information on how and when to resolve an address.
#[derive(Debug)]
pub struct Endpoint {
    pub url: crate::Url,
    pub timeouts: Option<RouteTimeouts>,
    pub retry: Option<RouteRetry>,
    pub address: EndpointAddress,
}

/// The address of an endpoint.
///
/// Depending on the type of endpoint, addresses may need to be further resolved by
/// a client implementation.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum EndpointAddress {
    /// A resolved IP address and port. This address can be used for a request
    /// without any further resolution.
    SocketAddr(SocketAddr),

    /// A DNS name and port. The name should be resolved periodically by the HTTP
    /// client and used to direct traffic.
    ///
    /// This name may be different than the hostname part of an [Endpoint]'s `url`.
    DnsName(String, u32),
}

impl<T> From<T> for EndpointAddress
where
    T: Into<SocketAddr>,
{
    fn from(t: T) -> Self {
        Self::SocketAddr(t.into())
    }
}

impl std::fmt::Display for EndpointAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointAddress::SocketAddr(addr) => addr.fmt(f),
            EndpointAddress::DnsName(name, port) => write!(f, "{name}:{port}"),
        }
    }
}

/// A snapshot of endpoint data.
pub struct EndpointIter {
    endpoint_group: Arc<EndpointGroup>,
}

impl From<Arc<EndpointGroup>> for EndpointIter {
    fn from(endpoint_group: Arc<EndpointGroup>) -> Self {
        Self { endpoint_group }
    }
}

// TODO: add a way to see endpoints grouped by locality. have to decide how
// to publicy expose Locality.
impl EndpointIter {
    /// Iterate over all of the addresses in this group, discarding any locality
    /// information.
    pub fn addrs(&self) -> impl Iterator<Item = &EndpointAddress> {
        self.endpoint_group.iter()
    }
}
