use std::str::FromStr;
use xds_api::pb::envoy::config::{
    cluster::v3::{self as xds_cluster, cluster::ring_hash_lb_config::HashFunction},
    core::v3 as xds_core,
    endpoint::v3 as xds_endpoint,
    route::v3 as xds_route,
};

use crate::{
    backend::{Backend, LbPolicy, RequestHashPolicy, RequestHasher, RingHashParams},
    error::{Error, ErrorContext},
    value_or_default,
    xds::ads_config_source,
    BackendId, Service,
};

impl Backend {
    pub fn from_xds(
        cluster: &xds_cluster::Cluster,
        route_action: Option<&xds_route::RouteAction>,
    ) -> Result<Self, Error> {
        use xds_cluster::cluster::DiscoveryType;

        let lb = LbPolicy::from_xds(cluster, route_action)?;
        let id = BackendId::from_str(&cluster.name)?;

        let discovery_type = cluster_discovery_type(cluster);

        match &id.service {
            // if this is supposed to be a DNS cluster, validate that the xDS
            // actually says it's a DNS cluster and that the discovery data
            // matches the name.
            Service::Dns(dns) => {
                if discovery_type != Some(DiscoveryType::LogicalDns) {
                    return Err(Error::new_static("mismatched discovery type"))
                        .with_field("cluster_discovery_type");
                }

                let addr_matches = logical_dns_address(cluster).is_some_and(|sa| {
                    sa.address == dns.hostname.as_ref()
                        && xds_port(sa.port_specifier.as_ref()) == Some(id.port)
                });
                if !addr_matches {
                    // NOTE: this error doesn't point at the actual proximate
                    // field because nobody has time for that. if this starts
                    // happening often, refine the error message here.
                    return Err(Error::new_static("cluster is not a valid DNS cluster"))
                        .with_fields("load_assignment", "endpoints");
                }
            }
            // kube clusters should use EDS over ADS, with a cluster_name matching cluster.name
            Service::Kube(_) => {
                if discovery_type != Some(DiscoveryType::Eds) {
                    return Err(Error::new_static("mismatched discovery type"))
                        .with_field("cluster_discovery_type");
                }

                let Some(eds_config) = &cluster.eds_cluster_config else {
                    return Err(Error::new_static("missing EDS config"))
                        .with_field("eds_cluster_conig");
                };

                if eds_config.service_name != id.name() {
                    return Err(Error::new_static("cluster_name must match EDS name"))
                        .with_fields("eds_cluster_config", "service_name");
                }

                if eds_config.eds_config != Some(ads_config_source()) {
                    return Err(Error::new_static("EDS cluster is not configured for ADS"))
                        .with_fields("eds_cluster_config", "eds_config");
                }
            }
        }

        Ok(Backend { id, lb })
    }

    pub fn to_xds(&self) -> xds_cluster::Cluster {
        use xds_cluster::cluster::ClusterDiscoveryType;
        use xds_cluster::cluster::DiscoveryType;
        use xds_cluster::cluster::EdsClusterConfig;

        let (lb_policy, lb_config) = match self.lb.to_xds() {
            Some((policy, config)) => (policy, Some(config)),
            None => (xds_cluster::cluster::LbPolicy::default(), None),
        };

        let (cluster_discovery_type, eds_cluster_config, load_assignment) = match &self.id.service {
            Service::Dns(dns) => {
                let dtype = ClusterDiscoveryType::Type(DiscoveryType::LogicalDns.into());
                let host_identifier = Some(xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(
                    xds_endpoint::Endpoint {
                        address: Some(to_xds_address(&dns.hostname, self.id.port)),
                        ..Default::default()
                    },
                ));
                let endpoints = vec![xds_endpoint::LocalityLbEndpoints {
                    lb_endpoints: vec![xds_endpoint::LbEndpoint {
                        host_identifier,
                        ..Default::default()
                    }],
                    ..Default::default()
                }];
                let load_assignment = Some(xds_endpoint::ClusterLoadAssignment {
                    endpoints,
                    ..Default::default()
                });
                (dtype, None, load_assignment)
            }
            Service::Kube(_) => {
                let cluster_discovery_type = ClusterDiscoveryType::Type(DiscoveryType::Eds.into());
                let eds_cluster_config = Some(EdsClusterConfig {
                    eds_config: Some(crate::xds::ads_config_source()),
                    service_name: self.id.name(),
                });
                (cluster_discovery_type, eds_cluster_config, None)
            }
        };

        let cluster_discovery_type = Some(cluster_discovery_type);
        xds_cluster::Cluster {
            name: self.id.name(),
            lb_policy: lb_policy.into(),
            lb_config,
            cluster_discovery_type,
            load_assignment,
            eds_cluster_config,
            ..Default::default()
        }
    }

    /// Generate a RouteConfiguration that routes to this Cluster using its
    /// hash_policy as RouteAction. This exists so we can guarantee that there's
    /// at least one unique RouteConfiguration pointing at every cluster so the
    /// client can deduce its hash policies that way.
    #[doc(hidden)]
    pub fn to_xds_lb_route_config(&self) -> xds_route::RouteConfiguration {
        use xds_route::route::Action;
        use xds_route::route_action::ClusterSpecifier;
        use xds_route::route_match::PathSpecifier;

        let default_action = Action::Route(xds_route::RouteAction {
            cluster_specifier: Some(ClusterSpecifier::Cluster(self.id.name())),
            hash_policy: self.to_xds_hash_policies(),
            ..Default::default()
        });

        let default_route = xds_route::Route {
            r#match: Some(xds_route::RouteMatch {
                path_specifier: Some(PathSpecifier::Prefix("".to_string())),
                ..Default::default()
            }),
            action: Some(default_action),
            ..Default::default()
        };

        let vhost = xds_route::VirtualHost {
            domains: vec!["*".to_string()],
            routes: vec![default_route],
            ..Default::default()
        };

        xds_route::RouteConfiguration {
            name: self.id.lb_config_route_name(),
            virtual_hosts: vec![vhost],
            ..Default::default()
        }
    }

    fn to_xds_hash_policies(&self) -> Vec<xds_route::route_action::HashPolicy> {
        match &self.lb {
            LbPolicy::RingHash(ring_hash) => {
                ring_hash.hash_params.iter().map(|p| p.to_xds()).collect()
            }
            _ => Vec::new(),
        }
    }
}

fn to_xds_address(hostname: &crate::Hostname, port: u16) -> xds_core::Address {
    let socket_address = xds_core::SocketAddress {
        address: hostname.to_string(),
        port_specifier: Some(xds_core::socket_address::PortSpecifier::PortValue(
            port as u32,
        )),
        ..Default::default()
    };

    xds_core::Address {
        address: Some(xds_core::address::Address::SocketAddress(socket_address)),
    }
}

fn cluster_discovery_type(
    cluster: &xds_cluster::Cluster,
) -> Option<xds_cluster::cluster::DiscoveryType> {
    match cluster.cluster_discovery_type {
        Some(xds_cluster::cluster::ClusterDiscoveryType::Type(cdt)) => {
            xds_cluster::cluster::DiscoveryType::try_from(cdt).ok()
        }
        _ => None,
    }
}

fn logical_dns_address(cluster: &xds_cluster::Cluster) -> Option<&xds_core::SocketAddress> {
    let cla = cluster.load_assignment.as_ref()?;
    let endpoint = cla.endpoints.first()?;
    let lb_endpoint = endpoint.lb_endpoints.first()?;

    let endpoint_addr = match lb_endpoint.host_identifier.as_ref()? {
        xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(endpoint) => {
            endpoint.address.as_ref()?
        }
        _ => return None,
    };

    match endpoint_addr.address.as_ref()? {
        xds_core::address::Address::SocketAddress(socket_address) => Some(socket_address),
        _ => None,
    }
}

#[inline]
fn xds_port(port_specifier: Option<&xds_core::socket_address::PortSpecifier>) -> Option<u16> {
    match port_specifier {
        Some(xds_core::socket_address::PortSpecifier::PortValue(v)) => (*v).try_into().ok(),
        _ => None,
    }
}

impl LbPolicy {
    pub(crate) fn from_xds(
        cluster: &xds_cluster::Cluster,
        route_action: Option<&xds_route::RouteAction>,
    ) -> Result<Self, Error> {
        match cluster.lb_policy() {
            // for ROUND_ROBIN, ignore the slow_start_config entirely and return a brand new
            // RoundRobin policy each time. validate that the config matches the enum field even
            // though it's ignored.
            xds_cluster::cluster::LbPolicy::RoundRobin => match cluster.lb_config.as_ref() {
                Some(xds_cluster::cluster::LbConfig::RoundRobinLbConfig(_)) => {
                    Ok(LbPolicy::RoundRobin)
                }
                None => Ok(LbPolicy::Unspecified),
                _ => Err(
                    Error::new_static("RoundRobin lb_policy has a mismatched lb_config")
                        .with_field("lb_config"),
                ),
            },
            // for RING_HASH pull the config out if set or use default values to populate our
            // config.
            xds_cluster::cluster::LbPolicy::RingHash => {
                let lb_config = match cluster.lb_config.as_ref() {
                    Some(xds_cluster::cluster::LbConfig::RingHashLbConfig(config)) => config,
                    None => &xds_cluster::cluster::RingHashLbConfig::default(),
                    _ => {
                        return Err(Error::new_static(
                            "RingHash lb_policy has a mismatched lb_config",
                        )
                        .with_field("lb_config"))
                    }
                };

                // hash function must be XX_HASH to match gRPC
                if lb_config.hash_function() != HashFunction::XxHash {
                    return Err(Error::new(format!(
                        "unsupported hash function: {:?}",
                        lb_config.hash_function(),
                    )))
                    .with_fields("lb_config", "hash_function");
                }

                let min_ring_size = value_or_default!(
                    lb_config.minimum_ring_size,
                    crate::backend::default_min_ring_size() as u64
                );
                let min_ring_size = min_ring_size
                    .try_into()
                    .map_err(|_| Error::new_static("int overflow"))
                    .with_fields("lb_config", ",minimum_ring_size")?;

                let hash_params = route_action
                    .map(hash_policies)
                    .transpose()
                    .with_field("route_action")?;

                Ok(LbPolicy::RingHash(RingHashParams {
                    min_ring_size,
                    hash_params: hash_params.unwrap_or_default(),
                }))
            }
            _ => Err(Error::new_static("unsupported lb policy")).with_field("lb_policy"),
        }
    }

    pub(crate) fn to_xds(
        &self,
    ) -> Option<(
        xds_cluster::cluster::LbPolicy,
        xds_cluster::cluster::LbConfig,
    )> {
        match self {
            LbPolicy::RoundRobin => Some((
                xds_cluster::cluster::LbPolicy::RoundRobin,
                xds_cluster::cluster::LbConfig::RoundRobinLbConfig(Default::default()),
            )),
            LbPolicy::RingHash(params) => Some((
                xds_cluster::cluster::LbPolicy::RingHash,
                xds_cluster::cluster::LbConfig::RingHashLbConfig(
                    xds_cluster::cluster::RingHashLbConfig {
                        minimum_ring_size: Some((params.min_ring_size as u64).into()),
                        hash_function:
                            xds_cluster::cluster::ring_hash_lb_config::HashFunction::XxHash as i32,
                        maximum_ring_size: None,
                    },
                ),
            )),
            // an unspecified LB policy just sets LbConfig to none
            LbPolicy::Unspecified => None,
        }
    }
}

#[inline]
fn hash_policies(action: &xds_route::RouteAction) -> Result<Vec<RequestHashPolicy>, Error> {
    let res: Result<Vec<_>, Error> = action
        .hash_policy
        .iter()
        .enumerate()
        .map(|(i, policy)| RequestHashPolicy::from_xds(policy).with_index(i))
        .collect();

    res.with_field("hash_policy")
}

impl RequestHashPolicy {
    pub(crate) fn to_xds(&self) -> xds_route::route_action::HashPolicy {
        use xds_route::route_action::hash_policy::{Header, PolicySpecifier, QueryParameter};

        let policy_specifier = match &self.hasher {
            RequestHasher::Header { name } => PolicySpecifier::Header(Header {
                header_name: name.clone(),
                regex_rewrite: None,
            }),
            RequestHasher::QueryParam { name } => {
                PolicySpecifier::QueryParameter(QueryParameter { name: name.clone() })
            }
        };

        xds_route::route_action::HashPolicy {
            terminal: self.terminal,
            policy_specifier: Some(policy_specifier),
        }
    }

    pub(crate) fn from_xds(xds: &xds_route::route_action::HashPolicy) -> Result<Self, Error> {
        use xds_route::route_action::hash_policy::PolicySpecifier;

        match &xds.policy_specifier {
            Some(PolicySpecifier::Header(header)) => Ok(Self {
                terminal: xds.terminal,
                hasher: RequestHasher::Header {
                    name: header.header_name.clone(),
                },
            }),
            Some(PolicySpecifier::QueryParameter(query)) => Ok(Self {
                terminal: xds.terminal,
                hasher: RequestHasher::QueryParam {
                    name: query.name.clone(),
                },
            }),
            Some(_) => {
                Err(Error::new_static("unsupported hash policy").with_field("policy_specifier"))
            }
            None => Err(Error::new_static("no policy specified").with_field("policy_specifier")),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_unspecified_lb_roundtrips() {
        let web = Service::kube("prod", "web").unwrap();

        let backend = Backend {
            id: web.as_backend_id(8891),
            lb: LbPolicy::Unspecified,
        };
        assert_eq!(backend, Backend::from_xds(&backend.to_xds(), None).unwrap(),);
        assert_eq!(backend.to_xds_hash_policies(), vec![]);
    }

    #[test]
    fn test_round_robin_lb_roundtrips() {
        let web = Service::kube("prod", "web").unwrap();

        let backend = Backend {
            id: web.as_backend_id(7891),
            lb: LbPolicy::RoundRobin,
        };
        assert_eq!(backend, Backend::from_xds(&backend.to_xds(), None).unwrap(),);
        assert_eq!(backend.to_xds_hash_policies(), vec![]);
    }

    #[test]
    fn test_ringhash_roundtrip() {
        let web = Service::kube("prod", "web").unwrap();

        let backend = Backend {
            id: web.as_backend_id(6666),
            lb: LbPolicy::RingHash(RingHashParams {
                min_ring_size: 1024,
                hash_params: vec![
                    RequestHashPolicy {
                        terminal: true,
                        hasher: RequestHasher::QueryParam {
                            name: "q".to_string(),
                        },
                    },
                    RequestHashPolicy {
                        terminal: false,
                        hasher: RequestHasher::Header {
                            name: "x-user".to_string(),
                        },
                    },
                    RequestHashPolicy {
                        terminal: true,
                        hasher: RequestHasher::Header {
                            name: "x-env".to_string(),
                        },
                    },
                ],
            }),
        };

        let cluster = backend.to_xds();
        let hash_policy = backend.to_xds_hash_policies();

        let parsed = Backend::from_xds(
            &cluster,
            Some(&xds_route::RouteAction {
                hash_policy,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(parsed, backend);
    }

    #[test]
    fn test_lb_route_config_roundtrip() {
        let web = Service::kube("prod", "web").unwrap();

        let backend = Backend {
            id: web.as_backend_id(12321),
            lb: LbPolicy::RingHash(RingHashParams {
                min_ring_size: 1024,
                hash_params: vec![
                    RequestHashPolicy {
                        terminal: false,
                        hasher: RequestHasher::Header {
                            name: "x-user".to_string(),
                        },
                    },
                    RequestHashPolicy {
                        terminal: false,
                        hasher: RequestHasher::Header {
                            name: "x-env".to_string(),
                        },
                    },
                ],
            }),
        };

        let cluster = backend.to_xds();
        let lb_config_route = backend.to_xds_lb_route_config();

        let parsed = Backend::from_xds(&cluster, {
            let vhost = lb_config_route.virtual_hosts.first().unwrap();
            let route = vhost.routes.first().unwrap();
            route.action.as_ref().map(|action| match action {
                xds_route::route::Action::Route(action) => action,
                _ => panic!("invalid route"),
            })
        })
        .unwrap();
        assert_eq!(backend, parsed);
    }
}
