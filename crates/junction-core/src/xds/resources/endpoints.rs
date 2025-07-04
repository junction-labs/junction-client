use std::net::SocketAddr;
use std::{collections::BTreeMap, net::IpAddr};

use xds_api::pb::envoy::config::{core::v3 as xds_core, endpoint::v3 as xds_endpoint};

use crate::hash::thread_local_xxhash;
use crate::xds::resources::ErrorCtx;

use super::{xds_port, Resource, ResourceError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoadAssignment {
    pub endpoints: Vec<EndpointGroup>,
}

#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub(crate) struct EndpointGroup {
    pub hash: u64,
    #[allow(unused)]
    pub priority: u32,
    pub endpoints: BTreeMap<Locality, Vec<SocketAddr>>,
}

impl EndpointGroup {
    pub(crate) fn new(priority: u32, endpoints: BTreeMap<Locality, Vec<SocketAddr>>) -> Self {
        let hash = thread_local_xxhash::hash(&endpoints);
        Self {
            hash,
            priority,
            endpoints,
        }
    }

    pub(crate) fn from_dns_addrs(addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
        let mut endpoints = BTreeMap::new();
        let endpoint_addrs = addrs.into_iter().collect();
        endpoints.insert(Locality::empty(), endpoint_addrs);
        Self::new(0, endpoints)
    }
}

// TODO: intern localities
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Locality {
    pub region: String,
    pub zone: String,
}

impl Locality {
    pub(crate) fn empty() -> Self {
        Locality {
            region: "".to_string(),
            zone: "".to_string(),
        }
    }

    fn from_xds(locality: &Option<xds_core::Locality>) -> Self {
        locality
            .as_ref()
            .map(|locality| Self {
                region: locality.region.clone(),
                zone: locality.zone.clone(),
            })
            .unwrap_or_else(Self::empty)
    }
}

impl Resource for LoadAssignment {
    type Xds = xds_endpoint::ClusterLoadAssignment;

    fn from_xds(xds: &Self::Xds) -> Result<Self, ResourceError> {
        // priority -> locality -> [addr]
        //
        // NOTE: this completely trusts the control plane not to send us
        // duplicate addresses or to reorder addresses in a meaningless way. if
        // endpoints are reordered, the hash of the EndpointGroup will not stay
        // stable and load balancing may be affected.
        let mut endpoints: BTreeMap<u32, BTreeMap<Locality, Vec<SocketAddr>>> = BTreeMap::new();

        for (endpoints_idx, locality_lb_endpoint) in xds.endpoints.iter().enumerate() {
            let priority = locality_lb_endpoint.priority;

            let locality = Locality::from_xds(&locality_lb_endpoint.locality);
            let priority_endpoints = endpoints.entry(priority).or_default();
            let locality_endpoints = priority_endpoints.entry(locality).or_default();

            for (lb_idx, lb_endpoint) in locality_lb_endpoint.lb_endpoints.iter().enumerate() {
                // skip all endpoints that are not HEALTHY or UNKNOWN
                if !matches!(
                    lb_endpoint.health_status(),
                    xds_core::HealthStatus::Healthy | xds_core::HealthStatus::Unknown,
                ) {
                    continue;
                }

                // parse the address and save it by locality/priority
                let socket_addr = xds_lb_endpoint_socket_addr(lb_endpoint)
                    .with_field_index("lb_endpoints", lb_idx)
                    .with_field_index("endpoints", endpoints_idx)?;
                locality_endpoints.push(socket_addr);
            }
        }

        // convert the endpoints map into an EndpointGroup. the inner
        // HashSet has to get converted to a Vec and the outer map becomes a
        // Vec of EndpointGroups.
        //
        // EndpointGroups are ordered by priority because they're in an ordered map
        let endpoints = endpoints.into_iter().map(|(priority, endpoints)| {
            let endpoints = endpoints
                .into_iter()
                .map(|(locality, addrs)| (locality, addrs.into_iter().collect()))
                .collect();
            EndpointGroup::new(priority, endpoints)
        });
        let endpoints: Vec<_> = endpoints.collect();
        // TODO: this assert isn't valid until MSRV is 1.82
        // assert!(
        //     endpoints.is_sorted_by_key(|e| e.priority),
        //     "EndpointGroups are ordered by priority: this is a bug in Junction",
        // );
        Ok(Self { endpoints })
    }

    fn references(&self) -> Vec<(super::ResourceType, super::ResourceName)> {
        Vec::new()
    }
}

impl EndpointGroup {
    pub(crate) fn len(&self) -> usize {
        self.endpoints.values().map(|v| v.len()).sum()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &SocketAddr> {
        self.endpoints.values().flatten()
    }

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

fn xds_lb_endpoint_socket_addr(
    endpoint: &xds_endpoint::LbEndpoint,
) -> Result<SocketAddr, ResourceError> {
    let endpoint = match &endpoint.host_identifier {
        Some(xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(ep)) => ep,
        _ => return Err(ResourceError::invalid("missing endpoint data")),
    };

    let address = endpoint.address.as_ref().and_then(|a| a.address.as_ref());
    match address {
        Some(xds_core::address::Address::SocketAddress(a)) => {
            let ip: IpAddr = a
                .address
                .parse()
                .map_err(|_| ResourceError::invalid("invalid socket address"))?;
            let port = xds_port(a.port_specifier.as_ref())
                .ok_or_else(|| ResourceError::invalid("invalid port"))?;

            Ok(SocketAddr::new(ip, port))
        }
        _ => Err(ResourceError::invalid("missing socket addr")),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use pretty_assertions::assert_eq;

    #[test]
    fn hash_huh() {
        assert_eq!(
            thread_local_xxhash::hash(&[1, 2, 3]),
            thread_local_xxhash::hash(&[1, 2, 3]),
        )
    }

    #[test]
    fn load_assignment_from_xds() {
        let endpoint = |addr: SocketAddr| {
            xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(xds_endpoint::Endpoint {
                address: Some(xds_core::Address {
                    address: Some(xds_core::address::Address::SocketAddress(
                        xds_core::SocketAddress {
                            address: addr.ip().to_string(),
                            port_specifier: Some(
                                xds_core::socket_address::PortSpecifier::PortValue(
                                    addr.port() as u32
                                ),
                            ),
                            ..Default::default()
                        },
                    )),
                }),
                ..Default::default()
            })
        };

        let load_assignment = xds_endpoint::ClusterLoadAssignment {
            cluster_name: "whatever.com:8008".to_string(),
            endpoints: vec![xds_endpoint::LocalityLbEndpoints {
                lb_endpoints: vec![
                    xds_endpoint::LbEndpoint {
                        host_identifier: Some(endpoint("192.168.194.79:8008".parse().unwrap())),
                        ..Default::default()
                    },
                    xds_endpoint::LbEndpoint {
                        host_identifier: Some(endpoint("192.168.194.80:8008".parse().unwrap())),
                        ..Default::default()
                    },
                    xds_endpoint::LbEndpoint {
                        host_identifier: Some(endpoint("192.168.194.81:8008".parse().unwrap())),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(
            LoadAssignment::from_xds(&load_assignment).unwrap(),
            LoadAssignment {
                endpoints: vec![EndpointGroup {
                    hash: 13400130612657829157,
                    priority: 0,
                    endpoints: BTreeMap::from_iter([(
                        Locality::empty(),
                        vec![
                            "192.168.194.79:8008".parse().unwrap(),
                            "192.168.194.80:8008".parse().unwrap(),
                            "192.168.194.81:8008".parse().unwrap(),
                        ]
                    )]),
                }],
            }
        )
    }
}
