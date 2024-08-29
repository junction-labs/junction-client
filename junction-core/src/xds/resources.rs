use std::{collections::BTreeSet, marker::PhantomData, sync::Arc};

use enum_map::EnumMap;
use xds_api::pb::google::protobuf;
use xds_api::{
    pb::envoy::{
        config::{
            cluster::v3::{self as xds_cluster},
            endpoint::v3::{self as xds_endpoint},
            listener::v3::{self as xds_listener},
            route::v3::{self as xds_route},
        },
        extensions::filters::network::http_connection_manager::v3 as xds_http,
    },
    WellKnownTypes,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq, enum_map::Enum, Hash)]
pub(crate) enum ResourceType {
    Listener,
    RouteConfiguration,
    Cluster,
    ClusterLoadAssignment,
}

impl ResourceType {
    fn as_well_known(&self) -> WellKnownTypes {
        match self {
            ResourceType::Listener => WellKnownTypes::Listener,
            ResourceType::RouteConfiguration => WellKnownTypes::RouteConfiguration,
            ResourceType::Cluster => WellKnownTypes::Cluster,
            ResourceType::ClusterLoadAssignment => WellKnownTypes::ClusterLoadAssignment,
        }
    }

    fn from_well_known(wkt: WellKnownTypes) -> Option<Self> {
        match wkt {
            WellKnownTypes::Listener => Some(Self::Listener),
            WellKnownTypes::RouteConfiguration => Some(Self::RouteConfiguration),
            WellKnownTypes::Cluster => Some(Self::Cluster),
            WellKnownTypes::ClusterLoadAssignment => Some(Self::ClusterLoadAssignment),
            _ => None,
        }
    }

    pub(crate) fn all() -> &'static [Self] {
        &[
            Self::Cluster,
            Self::ClusterLoadAssignment,
            Self::Listener,
            Self::RouteConfiguration,
        ]
    }

    pub(crate) fn type_url(&self) -> &'static str {
        self.as_well_known().type_url()
    }

    pub(crate) fn from_type_url(type_url: &str) -> Option<Self> {
        Self::from_well_known(WellKnownTypes::from_type_url(type_url)?)
    }
}

#[derive(Debug)]
pub(crate) enum ResourceVec {
    Listener(Vec<xds_listener::Listener>),
    RouteConfiguration(Vec<xds_route::RouteConfiguration>),
    Cluster(Vec<xds_cluster::Cluster>),
    ClusterLoadAssignment(Vec<xds_endpoint::ClusterLoadAssignment>),
}

impl ResourceVec {
    pub(crate) fn from_any(
        resource_type: ResourceType,
        any: Vec<protobuf::Any>,
    ) -> Result<Self, prost::DecodeError> {
        match resource_type {
            ResourceType::Listener => from_any_vec(any).map(Self::Listener),
            ResourceType::RouteConfiguration => from_any_vec(any).map(Self::RouteConfiguration),
            ResourceType::Cluster => from_any_vec(any).map(Self::Cluster),
            ResourceType::ClusterLoadAssignment => {
                from_any_vec(any).map(Self::ClusterLoadAssignment)
            }
        }
    }
}

fn from_any_vec<M: Default + prost::Name>(
    any: Vec<protobuf::Any>,
) -> Result<Vec<M>, prost::DecodeError> {
    // TODO: if the type_url checks become a bottleneck, we can only check once
    // and then decode every value instead of calling protobuf::Any::to_msg
    let mut ms = Vec::with_capacity(any.len());
    for a in any {
        ms.push(a.to_msg()?);
    }

    Ok(ms)
}

/// A specialized set of `ResourceType`s.
#[derive(Clone, Debug, Default)]
pub(crate) struct ResourceTypeSet(EnumMap<ResourceType, bool>);

impl ResourceTypeSet {
    pub(crate) fn len(&self) -> usize {
        self.0.values().filter(|x| **x).count()
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = ResourceType> {
        self.0.into_iter().filter_map(|(k, v)| v.then_some(k))
    }

    pub(crate) fn is_empty(&self) -> bool {
        !self.0.values().any(|e| *e)
    }

    pub(crate) fn insert(&mut self, resource_type: ResourceType) {
        self.0[resource_type] = true
    }
}

/// A typed reference to another resource.
///
/// This is functionally a `String` and implements `Clone`, `Ord`, and `Eq`` as
/// if it was just a string.
#[derive(Debug)]
pub(crate) struct ResourceName<T> {
    _type: PhantomData<T>,
    name: String,
}

impl<T> ResourceName<T> {
    pub fn as_str(&self) -> &str {
        &self.name
    }

    pub fn as_string(&self) -> String {
        self.name.clone()
    }
}

impl<T> Clone for ResourceName<T> {
    fn clone(&self) -> Self {
        Self {
            _type: PhantomData,
            name: self.name.clone(),
        }
    }
}

impl<T> From<String> for ResourceName<T> {
    fn from(name: String) -> Self {
        Self {
            _type: PhantomData,
            name,
        }
    }
}

impl<T> PartialEq for ResourceName<T> {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl<T> Eq for ResourceName<T> {}

#[allow(clippy::non_canonical_partial_ord_impl)]
impl<T> PartialOrd for ResourceName<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.name.partial_cmp(&other.name)
    }
}

impl<T> Ord for ResourceName<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

// TODO: filters go here
#[derive(Debug)]
pub(crate) struct ApiListener {
    pub name: String,
    pub xds: xds_http::HttpConnectionManager,
    pub data: ApiListenerData,
}

#[derive(Debug)]
pub(crate) enum ApiListenerData {
    RouteConfig {
        name: ResourceName<RouteConfig>,
    },
    Inlined {
        routes: Arc<Vec<crate::config::Route>>,
        clusters: Vec<ResourceName<Cluster>>,
    },
}

pub(crate) fn api_listener(
    listener: &xds_listener::Listener,
) -> Result<xds_http::HttpConnectionManager, crate::xds::Error> {
    let api_listener = listener
        .api_listener
        .as_ref()
        .and_then(|l| l.api_listener.as_ref())
        .ok_or_else(|| crate::xds::Error::InvalidXds {
            resource_type: "Listener",
            resource_name: listener.name.clone(),
            message: "Listener has no api_listener".to_string(),
        })?;

    api_listener
        .to_msg()
        .map_err(|e| crate::xds::Error::InvalidXds {
            resource_type: "Listener",
            resource_name: listener.name.clone(),
            message: format!("invalid HttpConnectionManager: {e}"),
        })
}

impl TryFrom<(String, xds_http::HttpConnectionManager)> for ApiListener {
    type Error = crate::xds::Error;

    fn try_from(
        (name, xds): (String, xds_http::HttpConnectionManager),
    ) -> Result<Self, Self::Error> {
        use xds_http::http_connection_manager::RouteSpecifier;

        let data = match &xds.route_specifier {
            Some(RouteSpecifier::Rds(rds_config)) => ApiListenerData::RouteConfig {
                name: rds_config.route_config_name.clone().into(),
            },
            Some(RouteSpecifier::RouteConfig(route_config)) => {
                let clusters = RouteConfig::cluster_names(route_config);
                let routes = RouteConfig::routes(route_config);
                ApiListenerData::Inlined { clusters, routes }
            }
            _ => {
                return Err(crate::xds::Error::InvalidXds {
                    resource_type: "HttpConnectionManager",
                    resource_name: name,
                    message: "HttpConnectionManager has no routes configured".to_string(),
                })
            }
        };

        Ok(Self { name, xds, data })
    }
}

#[derive(Debug)]
pub(crate) struct RouteConfig {
    pub name: String,
    pub xds: xds_route::RouteConfiguration,
    pub routes: Arc<Vec<crate::config::Route>>,
    pub clusters: Vec<ResourceName<Cluster>>,
}

impl RouteConfig {
    fn cluster_names(xds: &xds_route::RouteConfiguration) -> Vec<ResourceName<Cluster>> {
        let mut clusters = BTreeSet::new();
        for vhost in &xds.virtual_hosts {
            for route in &vhost.routes {
                let Some(xds_route::route::Action::Route(route_action)) = &route.action else {
                    continue;
                };

                match &route_action.cluster_specifier {
                    Some(xds_route::route_action::ClusterSpecifier::Cluster(cluster)) => {
                        clusters.insert(cluster.clone());
                    }
                    Some(xds_route::route_action::ClusterSpecifier::WeightedClusters(
                        weighted_clusters,
                    )) => {
                        for w in &weighted_clusters.clusters {
                            clusters.insert(w.name.clone());
                        }
                    }
                    _ => continue,
                }
            }
        }
        clusters.into_iter().map(|n| n.into()).collect()
    }

    fn routes(xds: &xds_route::RouteConfiguration) -> Arc<Vec<crate::config::Route>> {
        let routes = xds
            .virtual_hosts
            .iter()
            .map(crate::config::Route::from_xds)
            .collect();
        Arc::new(routes)
    }
}

impl TryFrom<xds_route::RouteConfiguration> for RouteConfig {
    type Error = crate::xds::Error;

    fn try_from(xds: xds_route::RouteConfiguration) -> Result<Self, Self::Error> {
        let name = xds.name.clone();
        let clusters = RouteConfig::cluster_names(&xds);
        let routes = RouteConfig::routes(&xds);

        Ok(Self {
            name,
            xds,
            routes,
            clusters,
        })
    }
}

// TODO: Figure out wtf to do to support logical_dns clusters.
#[derive(Debug)]
pub(crate) struct Cluster {
    pub name: String,
    pub xds: xds_cluster::Cluster,
    pub discovery_type: xds_cluster::cluster::DiscoveryType,
    pub load_balancer: Arc<crate::config::LoadBalancer>,
    pub data: ClusterData,
}

#[derive(Debug)]
pub(crate) enum ClusterData {
    #[allow(unused)]
    Inlined {
        name: String,
        endpoint_group: Arc<crate::config::EndpointGroup>,
    },
    LoadAssignment {
        name: ResourceName<LoadAssignment>,
    },
}

impl TryFrom<xds_cluster::Cluster> for Cluster {
    type Error = crate::xds::Error;

    // TOOD: support logical DNS clusters to keep parity with GRPC
    // FIXME: validate that the EDS config source uses ADS
    fn try_from(xds: xds_cluster::Cluster) -> Result<Self, Self::Error> {
        let Some(discovery_type) = cluster_discovery_type(&xds) else {
            return Err(crate::xds::Error::InvalidXds {
                resource_type: "Cluster",
                resource_name: xds.name,
                message: format!("invalid discovery_type: {:?}", xds.cluster_discovery_type),
            });
        };

        let load_assignment = match discovery_type {
            xds_cluster::cluster::DiscoveryType::Eds => {
                let Some(eds_config) = xds.eds_cluster_config.as_ref() else {
                    return Err(crate::xds::Error::InvalidXds {
                        resource_type: "Cluster",
                        resource_name: xds.name,
                        message: "an EDS cluster must have an eds_cluster_config".to_string(),
                    });
                };

                let cla_name = if !eds_config.service_name.is_empty() {
                    eds_config.service_name.clone()
                } else {
                    xds.name.clone()
                };
                cla_name.into()
            }
            _ => {
                return Err(crate::xds::Error::InvalidXds {
                    resource_type: "Cluster",
                    resource_name: xds.name,
                    message: "only EDS clusters are supported".to_string(),
                })
            }
        };

        let Some(load_balancer) = crate::config::load_balancer::LoadBalancer::from_xds(&xds) else {
            return Err(crate::xds::Error::InvalidXds {
                resource_type: "Cluster",
                resource_name: xds.name,
                message: "invalid LB config".to_string(),
            });
        };
        let load_balancer = Arc::new(load_balancer);

        let data = ClusterData::LoadAssignment {
            name: load_assignment,
        };

        Ok(Self {
            name: xds.name.clone(),
            xds,
            discovery_type,
            load_balancer,
            data,
        })
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

#[derive(Debug)]
pub(crate) struct LoadAssignment {
    pub name: String,
    pub xds: xds_endpoint::ClusterLoadAssignment,
    pub endpoint_group: Arc<crate::config::load_balancer::EndpointGroup>,
}

impl
    TryFrom<(
        xds_cluster::cluster::DiscoveryType,
        xds_endpoint::ClusterLoadAssignment,
    )> for LoadAssignment
{
    type Error = crate::xds::Error;

    fn try_from(
        (dtype, xds): (
            xds_cluster::cluster::DiscoveryType,
            xds_endpoint::ClusterLoadAssignment,
        ),
    ) -> Result<Self, Self::Error> {
        match crate::config::load_balancer::EndpointGroup::from_xds(&dtype, &xds) {
            Some(endpoint_group) => {
                let name = xds.cluster_name.clone();
                let endpoint_group = Arc::new(endpoint_group);
                Ok(Self {
                    name,
                    xds,
                    endpoint_group,
                })
            }
            None => Err(crate::xds::Error::InvalidXds {
                resource_type: "ClusterLoadAssignment",
                resource_name: xds.cluster_name,
                message: "invalid CLA".to_string(),
            }),
        }
    }
}
