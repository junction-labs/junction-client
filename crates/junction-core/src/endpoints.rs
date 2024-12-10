use junction_api::http::{RouteRetry, RouteTimeouts};
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

use crate::hash::thread_local_xxhash;

/// An HTTP endpoint to make a request to.
///
/// Endpoints contain both a target [url][crate::Url] that should be given to an
/// HTTP client and an address that indicates the address the the hostname in
/// the URL should resolve to.
#[derive(Debug)]
pub struct Endpoint {
    pub address: SocketAddr,
    pub url: crate::Url,
    pub timeouts: Option<RouteTimeouts>,
    pub retry: Option<RouteRetry>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Locality {
    Unknown,
    #[allow(unused)]
    Known(LocalityInfo),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LocalityInfo {
    pub(crate) region: String,
    pub(crate) zone: String,
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
    /// Iterate over all of the addresses in this group, without any locality
    /// information.
    pub fn addrs(&self) -> impl Iterator<Item = &SocketAddr> {
        self.endpoint_group.iter()
    }
}

#[derive(Debug, Default, Hash, PartialEq, Eq)]
pub(crate) struct EndpointGroup {
    pub(crate) hash: u64,
    endpoints: BTreeMap<Locality, Vec<SocketAddr>>,
}

impl EndpointGroup {
    pub(crate) fn new(endpoints: BTreeMap<Locality, Vec<SocketAddr>>) -> Self {
        let hash = thread_local_xxhash::hash(&endpoints);
        Self { hash, endpoints }
    }

    pub(crate) fn from_dns_addrs(addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
        let mut endpoints = BTreeMap::new();
        let endpoint_addrs = addrs.into_iter().collect();
        endpoints.insert(Locality::Unknown, endpoint_addrs);

        Self::new(endpoints)
    }

    pub(crate) fn len(&self) -> usize {
        self.endpoints.values().map(|v| v.len()).sum()
    }

    /// Returns an iterator over all endpoints in the group.
    ///
    /// Iteration order is guaranteed to be stable as long as the EndpointGroup is
    /// not modified, and guaranteed to consecutively produce all addresses in a single
    /// locality.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &SocketAddr> {
        self.endpoints.values().flatten()
    }

    /// Return the nth address in this group. The order
    pub(crate) fn nth(&self, n: usize) -> Option<&SocketAddr> {
        let mut n = n;
        for endpoints in self.endpoints.values() {
            if n < endpoints.len() {
                return Some(&endpoints[n]);
            }
            n -= endpoints.len();
        }

        None
    }
}
