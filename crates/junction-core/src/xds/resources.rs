use std::borrow::Cow;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::{collections::BTreeSet, marker::PhantomData, sync::Arc};

use enum_map::EnumMap;
use junction_api::backend::Backend;
use junction_api::http::Route;
use junction_api::{BackendId, Target};
use smol_str::SmolStr;
use xds_api::pb::google::protobuf;
use xds_api::{
    pb::envoy::{
        config::{
            cluster::v3 as xds_cluster, core::v3 as xds_core, endpoint::v3 as xds_endpoint,
            listener::v3 as xds_listener, route::v3 as xds_route,
        },
        extensions::filters::network::http_connection_manager::v3 as xds_http,
    },
    WellKnownTypes,
};

use crate::load_balancer::{EndpointGroup, LoadBalancer, Locality, LocalityInfo};
use crate::{BackendLb, EndpointAddress};

// FIXME: validate that the all the EDS config sources use ADS instead of just assuming it everywhere.

#[derive(Clone, Debug, thiserror::Error)]
pub(crate) enum ResourceError {
    #[error("{0}")]
    InvalidResource(#[from] junction_api::Error),

    #[error("invalid xDS discovery information")]
    InvalidXds {
        resource_name: String,
        message: Cow<'static, str>,
    },
}

impl ResourceError {
    fn for_xds(resource_name: String, message: String) -> Self {
        Self::InvalidXds {
            resource_name,
            message: message.into(),
        }
    }

    fn for_xds_static(resource_name: String, message: &'static str) -> Self {
        Self::InvalidXds {
            resource_name,
            message: message.into(),
        }
    }
}

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

    #[cfg(test)]
    pub(crate) fn names(&self) -> Vec<String> {
        macro_rules! clone_name {
            ($v:expr, $name:ident) => {
                $v.iter().map(|x| x.$name.clone()).collect()
            };
        }

        match self {
            ResourceVec::Listener(vec) => clone_name!(vec, name),
            ResourceVec::RouteConfiguration(vec) => clone_name!(vec, name),
            ResourceVec::Cluster(vec) => clone_name!(vec, name),
            ResourceVec::ClusterLoadAssignment(vec) => clone_name!(vec, cluster_name),
        }
    }

    #[cfg(test)]
    pub(crate) fn resource_type(&self) -> ResourceType {
        match self {
            ResourceVec::Listener(_) => ResourceType::Listener,
            ResourceVec::RouteConfiguration(_) => ResourceType::RouteConfiguration,
            ResourceVec::Cluster(_) => ResourceType::Cluster,
            ResourceVec::ClusterLoadAssignment(_) => ResourceType::ClusterLoadAssignment,
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

    pub(crate) fn contains(&self, resource_type: ResourceType) -> bool {
        self.0[resource_type]
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

/// an xDS RouteAction created by a Junction control plane exclusively for the
/// purpose of making it clear what the LB policy is for a specific backend.
///
/// the action is wrapped in an Arc so it can be grabbed from multiple paths
/// in the cache (listener update and cluster update) without being cloned
/// like mad everywhere.
#[derive(Clone, Debug)]
pub(crate) struct LbPolicyAction {
    pub cluster: ResourceName<Cluster>,
    pub route_action: Arc<xds_route::RouteAction>,
}

// TODO: filters go here
#[derive(Clone, Debug)]
pub(crate) struct ApiListener {
    pub xds: xds_listener::Listener,
    pub route_config: ApiListenerData,
}

#[derive(Clone, Debug)]
pub(crate) enum ApiListenerData {
    RouteConfig {
        name: ResourceName<RouteConfig>,
    },
    Inlined {
        route: Arc<Route>,
        clusters: Vec<ResourceName<Cluster>>,
        lb_action: Option<LbPolicyAction>,
    },
}

fn api_listener(
    listener: &xds_listener::Listener,
) -> Result<xds_http::HttpConnectionManager, ResourceError> {
    let api_listener = listener
        .api_listener
        .as_ref()
        .and_then(|l| l.api_listener.as_ref())
        .ok_or_else(|| {
            ResourceError::for_xds_static(listener.name.clone(), "Listener has no api_listener")
        })?;

    api_listener.to_msg().map_err(|e| {
        ResourceError::for_xds(listener.name.clone(), format!("invalid api_listener: {e}"))
    })
}

impl ApiListener {
    pub(crate) fn from_xds(name: &str, xds: xds_listener::Listener) -> Result<Self, ResourceError> {
        use xds_http::http_connection_manager::RouteSpecifier;

        let conn_manager = api_listener(&xds)?;
        let data = match &conn_manager.route_specifier {
            Some(RouteSpecifier::Rds(rds_config)) => ApiListenerData::RouteConfig {
                name: rds_config.route_config_name.clone().into(),
            },
            Some(RouteSpecifier::RouteConfig(route_config)) => {
                let clusters = RouteConfig::cluster_names(route_config);
                let route = Arc::new(Route::from_xds(route_config)?);
                // TODO: stop parsing lb_policy_action if this isn't tagged as a
                // policy passthrough listener/route.
                let lb_action = RouteConfig::lb_policy_action(route_config);
                ApiListenerData::Inlined {
                    clusters,
                    route,
                    lb_action,
                }
            }
            _ => {
                return Err(ResourceError::for_xds_static(
                    name.to_string(),
                    "api_listener has no routes configured",
                ))
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
    pub lb_action: Option<LbPolicyAction>,
}

impl RouteConfig {
    pub(crate) fn from_xds(
        xds: xds_route::RouteConfiguration,
    ) -> Result<Self, junction_api::Error> {
        let clusters = RouteConfig::cluster_names(&xds);
        let route = Arc::new(Route::from_xds(&xds)?);
        // TODO: stop parsing lb_policy_action if this isn't tagged as a
        // policy passthrough listener/route.
        let lb_action = RouteConfig::lb_policy_action(&xds);

        Ok(Self {
            xds,
            route,
            clusters,
            lb_action,
        })
    }

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

    fn lb_policy_action(xds: &xds_route::RouteConfiguration) -> Option<LbPolicyAction> {
        let vhost = match &xds.virtual_hosts.as_slice() {
            &[vhost] => vhost,
            _ => return None,
        };

        let route = match &vhost.routes.as_slice() {
            &[route] => route,
            _ => return None,
        };

        let Some(xds_route::route::Action::Route(route_action)) = &route.action else {
            return None;
        };
        match &route_action.cluster_specifier {
            Some(xds_route::route_action::ClusterSpecifier::Cluster(cluster)) => {
                Some(LbPolicyAction {
                    cluster: cluster.clone().into(),
                    route_action: Arc::new(route_action.clone()),
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Cluster {
    pub(crate) xds: xds_cluster::Cluster,
    pub(crate) backend_lb: Arc<BackendLb>,
}

impl Cluster {
    pub(crate) fn from_xds(
        xds: xds_cluster::Cluster,
        default_action: Option<&xds_route::RouteAction>,
    ) -> Result<Self, ResourceError> {
        let backend = Backend::from_xds(&xds, default_action)?;
        let load_balancer = LoadBalancer::from_config(&backend.lb);

        let backend_lb = Arc::new(BackendLb {
            config: backend,
            load_balancer,
        });

        Ok(Self { xds, backend_lb })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadAssignment {
    pub xds: xds_endpoint::ClusterLoadAssignment,
    pub endpoint_group: Arc<EndpointGroup>,
}

impl LoadAssignment {
    pub(crate) fn from_xds(target: BackendId, xds: xds_endpoint::ClusterLoadAssignment) -> Self {
        let endpoint_group = Arc::new(EndpointGroup::from_xds(&target, &xds));

        Self {
            xds,
            endpoint_group,
        }
    }
}

impl Locality {
    pub(crate) fn from_xds(locality: &Option<xds_core::Locality>) -> Self {
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

impl EndpointGroup {
    pub(crate) fn from_xds(target: &BackendId, cla: &xds_endpoint::ClusterLoadAssignment) -> Self {
        let make_address = match target.target {
            Target::Dns(_) => EndpointAddress::from_xds_dns_name,
            Target::KubeService(_) => EndpointAddress::from_xds_socket_addr,
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

        EndpointGroup::new(endpoints)
    }
}

impl EndpointAddress {
    pub(crate) fn from_xds_socket_addr(xds_address: &xds_core::SocketAddress) -> Option<Self> {
        let ip = xds_address.address.parse().ok()?;
        let port: u16 = match xds_address.port_specifier.as_ref()? {
            xds_core::socket_address::PortSpecifier::PortValue(port) => (*port).try_into().ok()?,
            _ => return None,
        };

        Some(Self::SocketAddr(SocketAddr::new(ip, port)))
    }

    pub(crate) fn from_xds_dns_name(xds_address: &xds_core::SocketAddress) -> Option<Self> {
        let address = xds_address.address.clone();
        let port = match xds_address.port_specifier.as_ref()? {
            xds_core::socket_address::PortSpecifier::PortValue(port) => port,
            _ => return None,
        };

        Some(Self::DnsName(address, *port))
    }

    pub(crate) fn from_xds_lb_endpoint<F>(endpoint: &xds_endpoint::LbEndpoint, f: F) -> Option<Self>
    where
        F: Fn(&xds_core::SocketAddress) -> Option<EndpointAddress>,
    {
        let endpoint = match endpoint.host_identifier.as_ref()? {
            xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(ep) => ep,
            xds_endpoint::lb_endpoint::HostIdentifier::EndpointName(_) => return None,
        };

        let address = endpoint.address.as_ref().and_then(|a| a.address.as_ref())?;
        match address {
            xds_core::address::Address::SocketAddress(socket_address) => f(socket_address),
            _ => None,
        }
    }
}
