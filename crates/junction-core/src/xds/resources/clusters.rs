use crate::{
    dns::DnsAddr,
    xds::{
        load_balancer::LoadBalancer,
        resources::{value_or_default, ErrorCtx},
    },
};

use super::{ads_config_source, xds_port, ResourceError, ResourceName, ResourceType};
use xds_api::pb::envoy::config::{
    cluster::v3 as xds_cluster, core::v3 as xds_core, endpoint::v3 as xds_endpoint,
};

#[derive(Clone, Debug, Default)]
pub(crate) enum LbPolicy {
    RoundRobin,
    RingHash {
        min_ring_size: u32,
    },
    #[default]
    Unspecified,
}

#[derive(Debug)]
pub(crate) struct Cluster {
    pub endpoints: Endpoints,
    // TODO: should we also keep LbPolicy around?
    pub load_balancer: LoadBalancer,
}

#[derive(Clone, Debug)]
pub(crate) enum Endpoints {
    Eds(ResourceName),
    LogicalDns { hostname: String, port: u16 },
    // TODO: Aggregate clusters
}

impl super::Resource for Cluster {
    type Xds = xds_cluster::Cluster;

    fn from_xds(xds: &Self::Xds) -> std::result::Result<Self, ResourceError> {
        let lb_policy = LbPolicy::from_xds(xds)?;

        let load_balancer = LoadBalancer::from_config(&lb_policy);
        let endpoints = Endpoints::from_xds(xds)?;
        Ok(Self {
            load_balancer,
            endpoints,
        })
    }

    fn references(&self) -> Vec<(super::ResourceType, ResourceName)> {
        match &self.endpoints {
            Endpoints::Eds(name) => vec![(ResourceType::ClusterLoadAssignment, name.clone())],
            _ => vec![],
        }
    }

    fn dns_names(&self) -> Vec<DnsAddr> {
        match &self.endpoints {
            Endpoints::LogicalDns { hostname, port } => vec![DnsAddr {
                hostname: hostname.clone(),
                port: *port,
            }],
            _ => vec![],
        }
    }
}

impl Endpoints {
    fn from_xds(cluster: &xds_cluster::Cluster) -> Result<Self, ResourceError> {
        let cluster_discovery_type = match cluster.cluster_discovery_type {
            Some(xds_cluster::cluster::ClusterDiscoveryType::Type(cdt)) => {
                xds_cluster::cluster::DiscoveryType::try_from(cdt).ok()
            }
            _ => None,
        };

        match &cluster_discovery_type {
            Some(xds_cluster::cluster::DiscoveryType::Eds) => {
                let cla_name = eds_service_name(cluster).with_field("eds_cluster_config")?;
                Ok(Self::Eds(cla_name))
            }
            Some(xds_cluster::cluster::DiscoveryType::LogicalDns) => {
                let (hostname, port) = logical_dns_name(cluster.load_assignment.as_ref())
                    .with_field("load_assignment")?;
                Ok(Self::LogicalDns { hostname, port })
            }
            Some(dt) => Err(ResourceError::invalid_with(format!(
                "unsupported discovery type: {dt:?}"
            )))
            .with_field("cluster_discovery_type"),
            None => Err(ResourceError::invalid("missing discovery type"))
                .with_field("cluster_discovery_type"),
        }
    }
}

fn eds_service_name(cluster: &xds_cluster::Cluster) -> Result<ResourceName, ResourceError> {
    let Some(eds_cluster_config) = &cluster.eds_cluster_config else {
        return Err(ResourceError::invalid("missing EDS config"));
    };
    if eds_cluster_config.eds_config != Some(ads_config_source()) {
        return Err(ResourceError::invalid("EDS cluster does not use ADS"));
    }

    Ok(if eds_cluster_config.service_name.is_empty() {
        ResourceName::from(cluster.name.clone())
    } else {
        ResourceName::from(eds_cluster_config.service_name.clone())
    })
}

// TODO: allow this to take a hostname/port? Figure out how and when GRPC does DNS resolution
fn logical_dns_name(
    cla: Option<&xds_endpoint::ClusterLoadAssignment>,
) -> Result<(String, u16), ResourceError> {
    let Some(cla) = cla else {
        return Err(ResourceError::invalid(
            "logical DNS cluster has no load assignment",
        ));
    };

    let [endpoint] = &cla.endpoints[..] else {
        return Err(ResourceError::invalid("must have exactly one endpoints"))
            .with_field("endpoints");
    };
    let [lb_endpoint] = &endpoint.lb_endpoints[..] else {
        return Err(ResourceError::invalid("must have exactly one lb endpoint"))
            .with_fields("endpoints", "lb_endpoints");
    };

    let endpoint = match &lb_endpoint.host_identifier {
        Some(xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(endpoint)) => Ok(endpoint),
        Some(xds_endpoint::lb_endpoint::HostIdentifier::EndpointName(_)) => Err(
            ResourceError::invalid("host identifier may not be an endpoint name"),
        ),
        None => Err(ResourceError::invalid("missing endpoint identifier")),
    };
    let endpoint = endpoint
        .with_field("host_identifier")
        .with_fields("endpoints", "lb_endpoint")?;

    let socket_addr = match &endpoint.address.as_ref().and_then(|a| a.address.as_ref()) {
        Some(xds_core::address::Address::SocketAddress(socket_addr)) => Ok(socket_addr),
        Some(_) => Err(ResourceError::invalid("address must be a socket address")),
        None => Err(ResourceError::invalid("missing address")),
    };
    let socket_addr = socket_addr
        .with_fields("host_identifier", "address")
        .with_fields("endpoints", "lb_endpoint")?;

    // FIXME: we should provide an error path here but things get so tedious and type-error-y
    // that we should probably fix the error API first.
    let hostname = socket_addr.address.clone();
    let port = xds_port(socket_addr.port_specifier.as_ref())
        .ok_or_else(|| ResourceError::invalid("invalid port"))?;

    Ok((hostname, port))
}

impl LbPolicy {
    pub(crate) fn from_xds(cluster: &xds_cluster::Cluster) -> Result<Self, ResourceError> {
        use xds_cluster::cluster::ring_hash_lb_config;

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
                    ResourceError::invalid("RoundRobin lb_policy has a mismatched lb_config")
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
                        return Err(ResourceError::invalid(
                            "RingHash lb_policy has a mismatched lb_config",
                        )
                        .with_field("lb_config"))
                    }
                };

                // hash function must be XX_HASH to match gRPC
                if lb_config.hash_function() != ring_hash_lb_config::HashFunction::XxHash {
                    return Err(ResourceError::invalid_with(format!(
                        "unsupported hash function: {:?}",
                        lb_config.hash_function(),
                    )))
                    .with_fields("lb_config", "hash_function");
                }

                let min_ring_size = value_or_default!(lb_config.minimum_ring_size, 1024);
                let min_ring_size = min_ring_size
                    .try_into()
                    .map_err(|_| ResourceError::invalid("integer overflow"))
                    .with_fields("lb_config", ",minimum_ring_size")?;

                Ok(LbPolicy::RingHash { min_ring_size })
            }
            _ => Err(ResourceError::invalid("unrecognized lb policy")).with_field("lb_policy"),
        }
    }
}
