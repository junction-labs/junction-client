use std::ops::Deref;
use std::{collections::BTreeSet, marker::PhantomData, sync::Arc};

use enum_map::EnumMap;
use junction_api_types::backend::Backend;
use junction_api_types::http::Route;
use junction_api_types::shared::Attachment;
use smol_str::SmolStr;
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

/// An opaque string used to version an xDS resource.
///
/// `ResourceVersion`s are immutable and cheap to `clone` and share.
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourceVersion(SmolStr);

impl Deref for ResourceVersion {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl serde::Serialize for ResourceVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl AsRef<str> for ResourceVersion {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

macro_rules! impl_resource_version_from {
    ($from_ty:ty) => {
        impl From<$from_ty> for ResourceVersion {
            fn from(s: $from_ty) -> ResourceVersion {
                ResourceVersion(s.into())
            }
        }
    };
}

impl_resource_version_from!(&str);
impl_resource_version_from!(&mut str);
impl_resource_version_from!(String);
impl_resource_version_from!(&String);
impl_resource_version_from!(Arc<str>);
impl_resource_version_from!(Box<str>);

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
#[derive(Clone, Debug)]
pub(crate) struct ApiListener {
    pub xds: xds_listener::Listener,
    pub route_config: ApiListenerRouteConfig,
}

#[derive(Clone, Debug)]
pub(crate) enum ApiListenerRouteConfig {
    RouteConfig {
        name: ResourceName<RouteConfig>,
    },
    Inlined {
        route: Arc<Route>,
        clusters: Vec<ResourceName<Cluster>>,
    },
}

fn api_listener(
    listener: &xds_listener::Listener,
) -> Result<xds_http::HttpConnectionManager, junction_api_types::xds::Error> {
    let api_listener = listener
        .api_listener
        .as_ref()
        .and_then(|l| l.api_listener.as_ref())
        .ok_or_else(|| junction_api_types::xds::Error::InvalidXds {
            resource_type: "Listener",
            resource_name: listener.name.clone(),
            message: "Listener has no api_listener".to_string(),
        })?;

    api_listener
        .to_msg()
        .map_err(|e| junction_api_types::xds::Error::InvalidXds {
            resource_type: "Listener",
            resource_name: listener.name.clone(),
            message: format!("invalid HttpConnectionManager: {e}"),
        })
}

impl ApiListener {
    pub(crate) fn from_xds(
        name: &str,
        xds: xds_listener::Listener,
    ) -> Result<Self, junction_api_types::xds::Error> {
        use xds_http::http_connection_manager::RouteSpecifier;

        let conn_manager = api_listener(&xds)?;
        let data = match &conn_manager.route_specifier {
            Some(RouteSpecifier::Rds(rds_config)) => ApiListenerRouteConfig::RouteConfig {
                name: rds_config.route_config_name.clone().into(),
            },
            Some(RouteSpecifier::RouteConfig(route_config)) => {
                let clusters = RouteConfig::cluster_names(route_config);
                let route = RouteConfig::route(route_config)?;
                ApiListenerRouteConfig::Inlined { clusters, route }
            }
            _ => {
                return Err(junction_api_types::xds::Error::InvalidXds {
                    resource_name: name.to_string(),
                    resource_type: "HttpConnectionManager",
                    message: "HttpConnectionManager has no routes configured".to_string(),
                })
            }
        };

        Ok(Self {
            xds,
            route_config: data,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RouteConfig {
    pub xds: xds_route::RouteConfiguration,
    pub route: Arc<Route>,
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

    fn route(
        xds: &xds_route::RouteConfiguration,
    ) -> Result<Arc<Route>, junction_api_types::xds::Error> {
        let route = Route::from_xds(xds)?;
        Ok(Arc::new(route))
    }
}

impl RouteConfig {
    pub(crate) fn from_xds(
        xds: xds_route::RouteConfiguration,
    ) -> Result<Self, junction_api_types::xds::Error> {
        let clusters = RouteConfig::cluster_names(&xds);
        let route = RouteConfig::route(&xds)?;

        Ok(Self {
            xds,
            route,
            clusters,
        })
    }
}

// TODO: Figure out wtf to do to support logical_dns clusters.
#[derive(Clone, Debug)]
pub(crate) struct Cluster {
    pub xds: xds_cluster::Cluster,
    pub config: Backend,
    pub load_balancer: Arc<crate::config::LoadBalancer>,
    pub endpoints: ClusterEndpointData,
}

#[derive(Clone, Debug)]
pub(crate) enum ClusterEndpointData {
    #[allow(unused)]
    Inlined {
        name: String,
        endpoint_group: Arc<crate::config::EndpointGroup>,
    },
    LoadAssignment {
        name: ResourceName<LoadAssignment>,
    },
}

impl Cluster {
    // TODO: support logical DNS clusters to keep parity with GRPC
    // FIXME: validate that the EDS config source uses ADS
    pub(crate) fn from_xds(
        xds: xds_cluster::Cluster,
    ) -> Result<Self, junction_api_types::xds::Error> {
        let backend = Backend::from_xds(&xds)?;

        let load_balancer = Arc::new(crate::config::load_balancer::LoadBalancer::from_config(
            &backend.lb,
        ));

        let data = ClusterEndpointData::LoadAssignment {
            name: backend.attachment.get_cluster_xds_name().into(),
        };

        Ok(Self {
            xds,
            config: backend,
            load_balancer,
            endpoints: data,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadAssignment {
    pub xds: xds_endpoint::ClusterLoadAssignment,
    pub endpoint_group: Arc<crate::config::load_balancer::EndpointGroup>,
}

impl LoadAssignment {
    pub(crate) fn from_xds(
        attachment: Attachment,
        xds: xds_endpoint::ClusterLoadAssignment,
    ) -> Result<Self, junction_api_types::xds::Error> {
        match crate::config::load_balancer::EndpointGroup::from_xds(&attachment, &xds) {
            Some(endpoint_group) => {
                let endpoint_group = Arc::new(endpoint_group);
                Ok(Self {
                    xds,
                    endpoint_group,
                })
            }
            None => Err(junction_api_types::xds::Error::InvalidXds {
                resource_type: "ClusterLoadAssignment",
                resource_name: xds.cluster_name,
                message: "invalid CLA".to_string(),
            }),
        }
    }
}
