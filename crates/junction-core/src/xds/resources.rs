use std::borrow::Cow;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::{collections::BTreeSet, marker::PhantomData, sync::Arc};

use enum_map::EnumMap;
use junction_api::backend::Backend;
use junction_api::backend::BackendId;
use junction_api::http::Route;
use smol_str::SmolStr;
use xds_api::pb::google::protobuf;
use xds_api::{
    pb::envoy::{
        config::{
            cluster::v3 as xds_cluster, core::v3 as xds_core, endpoint::v3 as xds_endpoint,
            listener::v3 as xds_listener, route::v3 as xds_route,
        },
        extensions::filters::network::http_connection_manager::v3 as xds_http,
        service::discovery::v3 as xds_discovery,
    },
    WellKnownTypes,
};

use crate::endpoints::{EndpointGroup, Locality, LocalityInfo};
use crate::load_balancer::{BackendLb, LoadBalancer};

// FIXME: validate that the all the EDS config sources use ADS instead of just assuming it everywhere.

#[derive(Clone, Debug, thiserror::Error)]
pub(crate) enum ResourceError {
    #[error("{0}")]
    InvalidResource(#[from] junction_api::Error),

    #[error("invalid xDS: {resource_name}: {message}")]
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

/// The type of an xDS resource we store in cache.
///
/// The order these are declared in is the xDS make-before-break order, so that
/// the [enum_map] crate keeps enum maps in this order. This means any time we
/// need to iterate an EnumMap's values, we're probably doing it in an order
/// that keeps state updates sane.
#[derive(Debug, Copy, Clone, PartialEq, Eq, enum_map::Enum, Hash, PartialOrd, Ord)]
pub(crate) enum ResourceType {
    Cluster,
    ClusterLoadAssignment,
    Listener,
    RouteConfiguration,
}

impl ResourceType {
    fn as_well_known(&self) -> WellKnownTypes {
        match self {
            ResourceType::Cluster => WellKnownTypes::Cluster,
            ResourceType::ClusterLoadAssignment => WellKnownTypes::ClusterLoadAssignment,
            ResourceType::Listener => WellKnownTypes::Listener,
            ResourceType::RouteConfiguration => WellKnownTypes::RouteConfiguration,
        }
    }

    fn from_well_known(wkt: WellKnownTypes) -> Option<Self> {
        match wkt {
            WellKnownTypes::Cluster => Some(Self::Cluster),
            WellKnownTypes::ClusterLoadAssignment => Some(Self::ClusterLoadAssignment),
            WellKnownTypes::Listener => Some(Self::Listener),
            WellKnownTypes::RouteConfiguration => Some(Self::RouteConfiguration),
            _ => None,
        }
    }

    pub(crate) const fn supports_wildcard(&self) -> bool {
        match self {
            ResourceType::Cluster => true,
            ResourceType::Listener => true,
            _ => false,
        }
    }

    /// Return all of the known enum variants in xDS's make-before-break order.
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

#[derive(Clone, Debug)]
pub(crate) enum ResourceVec {
    Listener(VersionedVec<xds_listener::Listener>),
    RouteConfiguration(VersionedVec<xds_route::RouteConfiguration>),
    Cluster(VersionedVec<xds_cluster::Cluster>),
    ClusterLoadAssignment(VersionedVec<xds_endpoint::ClusterLoadAssignment>),
}

type VersionedVec<T> = Vec<(ResourceVersion, T)>;

impl ResourceVec {
    pub(crate) fn from_any(
        rtype: ResourceType,
        version: ResourceVersion,
        any: Vec<protobuf::Any>,
    ) -> Result<Self, prost::DecodeError> {
        match rtype {
            ResourceType::Cluster => from_any_vec(version, any).map(ResourceVec::Cluster),
            ResourceType::ClusterLoadAssignment => {
                from_any_vec(version, any).map(Self::ClusterLoadAssignment)
            }
            ResourceType::Listener => from_any_vec(version, any).map(Self::Listener),
            ResourceType::RouteConfiguration => {
                from_any_vec(version, any).map(Self::RouteConfiguration)
            }
        }
    }

    pub(crate) fn from_resources(
        rtype: ResourceType,
        resources: Vec<xds_discovery::Resource>,
    ) -> Result<Self, prost::DecodeError> {
        match rtype {
            ResourceType::Cluster => from_resource_vec(resources).map(Self::Cluster),
            ResourceType::ClusterLoadAssignment => {
                from_resource_vec(resources).map(Self::ClusterLoadAssignment)
            }
            ResourceType::Listener => from_resource_vec(resources).map(Self::Listener),
            ResourceType::RouteConfiguration => {
                from_resource_vec(resources).map(Self::RouteConfiguration)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn names(&self) -> Vec<String> {
        macro_rules! clone_name {
            ($v:expr, $name:ident) => {
                $v.iter().map(|(_, x)| x.$name.clone()).collect()
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

    #[cfg(test)]
    pub(crate) fn to_resources(&self) -> Result<Vec<xds_discovery::Resource>, prost::EncodeError> {
        match self {
            ResourceVec::Listener(vec) => to_resource_vec(vec),
            ResourceVec::RouteConfiguration(vec) => to_resource_vec(vec),
            ResourceVec::Cluster(vec) => to_resource_vec(vec),
            ResourceVec::ClusterLoadAssignment(vec) => to_resource_vec(vec),
        }
    }
}

macro_rules! test_constructor {
    ($name:ident, $variant:ident, $xds_type:ty) => {
        impl ResourceVec {
            #[cfg(test)]
            pub(crate) fn $name<I: IntoIterator<Item = $xds_type>>(
                version: ResourceVersion,
                xs: I,
            ) -> Self {
                let data = xs.into_iter().map(|l| (version.clone(), l)).collect();
                Self::$variant(data)
            }
        }
    };
}

test_constructor!(from_listeners, Listener, xds_listener::Listener);
test_constructor!(
    from_route_configs,
    RouteConfiguration,
    xds_route::RouteConfiguration
);
test_constructor!(from_clusters, Cluster, xds_cluster::Cluster);
test_constructor!(
    from_load_assignments,
    ClusterLoadAssignment,
    xds_endpoint::ClusterLoadAssignment
);

fn from_resource_vec<M: Default + prost::Name>(
    resources: Vec<xds_discovery::Resource>,
) -> Result<VersionedVec<M>, prost::DecodeError> {
    let mut ms = Vec::with_capacity(resources.len());
    for r in resources {
        let Some(any) = r.resource else {
            continue;
        };
        ms.push((ResourceVersion::from(r.version), any.to_msg()?));
    }

    Ok(ms)
}

#[cfg(test)]
fn to_resource_vec<M: prost::Name>(
    xs: &VersionedVec<M>,
) -> Result<Vec<xds_discovery::Resource>, prost::EncodeError> {
    let mut resources = Vec::with_capacity(xs.len());

    for (v, msg) in xs {
        let as_any = protobuf::Any::from_msg(msg)?;

        resources.push(xds_discovery::Resource {
            resource: Some(as_any),
            version: v.to_string(),
            ..Default::default()
        })
    }

    Ok(resources)
}

fn from_any_vec<M: Default + prost::Name>(
    version: ResourceVersion,
    any: Vec<protobuf::Any>,
) -> Result<VersionedVec<M>, prost::DecodeError> {
    // TODO: if the type_url checks become a bottleneck, we can only check once
    // and then decode every value instead of calling protobuf::Any::to_msg
    let mut ms = Vec::with_capacity(any.len());
    for a in any {
        ms.push((version.clone(), a.to_msg()?));
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

#[derive(Clone, Debug)]
pub(crate) struct ApiListener {
    pub xds: xds_listener::Listener,
    pub route_config: ApiListenerData,
}

#[derive(Clone, Debug)]
pub(crate) enum ApiListenerData {
    Rds(ResourceName<RouteConfig>),
    Inlined(RouteConfigData),
}

fn http_connection_manager(
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

        let conn_manager = http_connection_manager(&xds)?;
        let data = match &conn_manager.route_specifier {
            Some(RouteSpecifier::Rds(rds)) => {
                let name = rds.route_config_name.clone();
                ApiListenerData::Rds(name.into())
            }
            Some(RouteSpecifier::RouteConfig(route_config)) => {
                let data = RouteConfigData::from_xds(route_config)?;
                ApiListenerData::Inlined(data)
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
    pub data: RouteConfigData,
}

#[derive(Clone, Debug)]
pub(super) enum RouteConfigData {
    Route {
        route: Arc<Route>,
        clusters: Vec<ResourceName<Cluster>>,
    },
    LbPolicy {
        action: Arc<xds_route::RouteAction>,
        cluster: ResourceName<Cluster>,
    },
}

impl RouteConfigData {
    fn from_xds(xds: &xds_route::RouteConfiguration) -> Result<Self, ResourceError> {
        match BackendId::from_lb_config_route_name(&xds.name) {
            // it's a normal route
            Err(_) => {
                let clusters = RouteConfig::cluster_names(xds);
                let route = Arc::new(Route::from_xds(xds)?);
                Ok(RouteConfigData::Route { route, clusters })
            }
            // it's an lb config route
            Ok(_) => RouteConfig::lb_policy_action(xds).ok_or(ResourceError::for_xds_static(
                xds.name.clone(),
                "failed to parse LB config route",
            )),
        }
    }
}

impl RouteConfig {
    pub(crate) fn from_xds(xds: xds_route::RouteConfiguration) -> Result<Self, ResourceError> {
        let data = RouteConfigData::from_xds(&xds)?;
        Ok(Self { xds, data })
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

    fn lb_policy_action(xds: &xds_route::RouteConfiguration) -> Option<RouteConfigData> {
        let vhost = match &xds.virtual_hosts.as_slice() {
            &[vhost] => vhost,
            _ => return None,
        };

        let route = match &vhost.routes.as_slice() {
            &[route] => route,
            _ => return None,
        };

        let Some(xds_route::route::Action::Route(action)) = &route.action else {
            return None;
        };
        match &action.cluster_specifier {
            Some(xds_route::route_action::ClusterSpecifier::Cluster(cluster)) => {
                Some(RouteConfigData::LbPolicy {
                    action: Arc::new(action.clone()),
                    cluster: cluster.clone().into(),
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
    pub(crate) fn id(&self) -> &BackendId {
        &self.backend_lb.config.id
    }
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
    pub(crate) fn from_xds(
        xds: xds_endpoint::ClusterLoadAssignment,
    ) -> Result<Self, ResourceError> {
        let endpoint_group = Arc::new(EndpointGroup::from_xds(&xds)?);
        Ok(Self {
            xds,
            endpoint_group,
        })
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
    pub(crate) fn from_xds(
        cla: &xds_endpoint::ClusterLoadAssignment,
    ) -> Result<Self, ResourceError> {
        let mut endpoints = BTreeMap::new();
        for (locality_idx, locality_endpoints) in cla.endpoints.iter().enumerate() {
            let locality = Locality::from_xds(&locality_endpoints.locality);
            let locality_endpoints: Result<Vec<_>, _> = locality_endpoints
                .lb_endpoints
                .iter()
                .enumerate()
                .map(|(endpoint_idx, e)| {
                    xds_lb_endpoint_socket_addr(&cla.cluster_name, locality_idx, endpoint_idx, e)
                })
                .collect();

            endpoints.insert(locality, locality_endpoints?);
        }

        Ok(EndpointGroup::new(endpoints))
    }
}

fn xds_lb_endpoint_socket_addr(
    eds_name: &str,
    locality_idx: usize,
    endpoint_idx: usize,
    endpoint: &xds_endpoint::LbEndpoint,
) -> Result<SocketAddr, ResourceError> {
    macro_rules! make_error {
        ($msg:expr) => {
            ResourceError::for_xds_static(
                format!(
                    "{}: endpoints[{}].lb_endpoints[{}]",
                    eds_name, locality_idx, endpoint_idx
                ),
                $msg,
            )
        };
    }

    let endpoint = match &endpoint.host_identifier {
        Some(xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(ep)) => ep,
        _ => return Err(make_error!("endpoint is missing endpoint data")),
    };

    let address = endpoint.address.as_ref().and_then(|a| a.address.as_ref());
    match address {
        Some(xds_core::address::Address::SocketAddress(addr)) => {
            let ip = addr
                .address
                .parse()
                .map_err(|_| make_error!("invalid socket address"))?;
            let port = match &addr.port_specifier {
                Some(xds_core::socket_address::PortSpecifier::PortValue(p)) => {
                    (*p).try_into().map_err(|_| make_error!("invalid port"))?
                }
                _ => return Err(make_error!("missing port specifier")),
            };

            Ok(SocketAddr::new(ip, port))
        }
        _ => Err(make_error!("endpoint has no socket address")),
    }
}
