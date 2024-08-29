use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};
use xds_api::pb::envoy::config::{
    cluster::v3 as xds_cluster, core::v3 as xds_core, endpoint::v3 as xds_endpoint,
};

use crate::EndpointAddress;

// FIXME: need a way to produce RequestContext from a route.

#[derive(Debug)]
pub(crate) struct EndpointGroup {
    endpoints: BTreeMap<Locality, Vec<crate::EndpointAddress>>,
}

impl EndpointGroup {
    pub(crate) fn from_xds(
        cluster_type: &xds_cluster::cluster::DiscoveryType,
        cla: &xds_endpoint::ClusterLoadAssignment,
    ) -> Option<Self> {
        use xds_cluster::cluster::DiscoveryType;

        let make_address = match cluster_type {
            DiscoveryType::Static | DiscoveryType::Eds => EndpointAddress::from_socket_addr,
            _ => EndpointAddress::from_dns_name,
        };

        let mut endpoints = BTreeMap::new();
        for locality_endpoints in &cla.endpoints {
            let locality = Locality::from_xds(&locality_endpoints.locality);
            let locality_endpoints: Vec<_> = locality_endpoints
                .lb_endpoints
                .iter()
                .filter_map(|endpoint| {
                    crate::EndpointAddress::from_xds_lb_endpoint(endpoint, make_address)
                })
                .collect();

            endpoints.insert(locality, locality_endpoints);
        }

        Some(Self { endpoints })
    }
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

impl Locality {
    fn from_xds(locality: &Option<xds_core::Locality>) -> Self {
        let Some(locality) = locality.as_ref() else {
            return Self::Unknown;
        };

        if locality.region.is_empty() && locality.zone.is_empty() {
            return Self::Unknown;
        }

        Self::Known(LocalityInfo {
            region: locality.region.clone(),
            zone: locality.zone.clone(),
        })
    }
}

pub(crate) struct RequestContext;

#[derive(Debug)]
pub(crate) enum LoadBalancer {
    RoundRobin {
        idx: AtomicUsize,
    },

    #[allow(unused)]
    RingHash {
        min_size: u64,
        max_size: u64,
    },
}

impl LoadBalancer {
    pub(crate) fn load_balance<'ep>(
        &self,
        _ctx: &RequestContext,
        locality_endpoints: &'ep EndpointGroup,
    ) -> Option<&'ep crate::EndpointAddress> {
        match self {
            // TODO: when doing weighted round robin, it's worth adapting the GRPC
            // scheduling in static_stride_scheduler.cc instead of inventing a new technique
            // ourselves.
            //
            // src/core/load_balancing/weighted_round_robin/static_stride_scheduler.cc
            LoadBalancer::RoundRobin { idx } => {
                let endpoints = locality_endpoints.endpoints.get(&Locality::Unknown)?;
                let locality_idx = idx.fetch_add(1, Ordering::SeqCst) % endpoints.len();
                Some(&endpoints[locality_idx])
            }
            LoadBalancer::RingHash { .. } => todo!("implement ring hashing"),
        }
    }
}

impl LoadBalancer {
    pub(crate) fn from_xds(cluster: &xds_cluster::Cluster) -> Option<Self> {
        let lb_policy = xds_cluster::cluster::LbPolicy::try_from(cluster.lb_policy).ok()?;

        match lb_policy {
            // for ROUND_ROBIN, ignore the slow_start_config entirely and return
            // a brand new RoundRobin policy each time. validate that the config
            // matches the enum field even though it's ignored.
            xds_cluster::cluster::LbPolicy::RoundRobin => {
                match cluster.lb_config.as_ref() {
                    Some(xds_cluster::cluster::LbConfig::RoundRobinLbConfig(_)) | None => (),
                    _ => return None,
                };

                Some(LoadBalancer::RoundRobin {
                    idx: AtomicUsize::new(0),
                })
            }
            // for RING_HASH pull the config out if set or use default values to
            // populate our config.
            xds_cluster::cluster::LbPolicy::RingHash => {
                let lb_config = match cluster.lb_config.as_ref() {
                    Some(xds_cluster::cluster::LbConfig::RingHashLbConfig(config)) => config,
                    None => &xds_cluster::cluster::RingHashLbConfig::default(),
                    _ => return None,
                };

                let min_size = crate::xds::value_or_default!(lb_config.minimum_ring_size, 2048);
                let max_size = crate::xds::value_or_default!(lb_config.maximum_ring_size, 4096);
                Some(LoadBalancer::RingHash { min_size, max_size })
            }
            _ => None,
        }
    }
}
