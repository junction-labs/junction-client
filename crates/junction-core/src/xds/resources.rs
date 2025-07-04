pub(crate) mod clusters;
pub(crate) mod endpoints;
pub(crate) mod listeners;
pub(crate) mod route_configs;
pub(crate) use clusters::Cluster;
pub(crate) use endpoints::LoadAssignment;
pub(crate) use listeners::ApiListener;
pub(crate) use route_configs::RouteConfiguration;

use smol_str::SmolStr;
use std::borrow::Cow;
use std::fmt::Write;
use std::ops::Deref;
use std::sync::Arc;

use xds_api::{
    pb::{envoy::config::core::v3 as xds_core, google::protobuf},
    WellKnownTypes,
};

macro_rules! value_or_default {
    ($value:expr, $default:expr) => {
        $value.as_ref().map(|v| v.value).unwrap_or($default)
    };
}
pub(crate) use value_or_default;

use crate::dns::DnsAddr;

// FIXME: validate that the all the EDS config sources use ADS instead of just assuming it everywhere.

/// An xDS resource that can be handled, decoded, and cached.
///
/// Resources must be able to be decoded from a [protobuf::Any] and from
/// their associated xDS protobuf type, and should know how to return a
/// set of other xDS resources they reference.
pub(crate) trait Resource: Sized {
    type Xds: prost::Name + Default;

    fn from_any(any: &protobuf::Any) -> Result<Self, ResourceError> {
        let m: Self::Xds = any.to_msg()?;
        Self::from_xds(&m)
    }

    fn from_xds(xds: &Self::Xds) -> Result<Self, ResourceError>;
    fn references(&self) -> Vec<(ResourceType, ResourceName)>;
    fn dns_names(&self) -> Vec<DnsAddr> {
        Vec::new()
    }
}

/// An error that occurred while trying to parse and validate an incoming
/// xDS message.
#[derive(Clone, Debug, thiserror::Error, PartialEq)]
pub(crate) struct ResourceError {
    kind: ResourceErrorKind,
    path: Vec<PathEntry>,
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.path.is_empty() {
            write!(f, "{}: ", path_str(&self.path))?;
        }

        write!(f, "{}", self.kind)
    }
}

impl From<prost::DecodeError> for ResourceError {
    fn from(err: prost::DecodeError) -> Self {
        Self {
            kind: ResourceErrorKind::Decode(err),
            path: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
enum ResourceErrorKind {
    #[error(transparent)]
    Decode(prost::DecodeError),

    #[error("{0}")]
    Invalid(Cow<'static, str>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PathEntry {
    Field(&'static str),
    Index(usize),
    MapIndex(String),
}

impl std::fmt::Display for PathEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathEntry::Field(field) => f.write_str(field),
            PathEntry::Index(idx) => write!(f, "[{idx}]"),
            PathEntry::MapIndex(field) => write!(f, "[{field}]"),
        }
    }
}

fn path_str(path: &[PathEntry]) -> String {
    let mut buf = String::new();

    let path_iter = path.iter().rev();
    for (i, path_entry) in path_iter.enumerate() {
        if i > 0 && matches!(path_entry, PathEntry::Field(_)) {
            buf.push('.');
        }
        let _ = write!(&mut buf, "{path_entry}");
    }

    buf
}

#[allow(unused)]
impl ResourceError {
    pub(crate) fn invalid(msg: &'static str) -> Self {
        Self {
            kind: ResourceErrorKind::Invalid(Cow::Borrowed(msg)),
            path: Vec::new(),
        }
    }

    pub(crate) fn invalid_with(msg: String) -> Self {
        Self {
            kind: ResourceErrorKind::Invalid(Cow::Owned(msg)),
            path: Vec::new(),
        }
    }

    pub(crate) fn with_field(mut self, field: &'static str) -> Self {
        self.path.push(PathEntry::Field(field));
        self
    }

    pub(crate) fn with_index(mut self, idx: usize) -> Self {
        self.path.push(PathEntry::Index(idx));
        self
    }

    #[inline(always)]
    pub(crate) fn with_field_index(self, field: &'static str, index: usize) -> Self {
        self.with_index(index).with_field(field)
    }

    pub(crate) fn with_map_key(mut self, key: String) -> Self {
        self.path.push(PathEntry::MapIndex(key));
        self
    }

    #[inline(always)]
    pub(crate) fn with_field_key<T: std::fmt::Display>(self, field: &'static str, key: T) -> Self {
        self.with_map_key(key.to_string()).with_field(field)
    }
}

// use a trait here so we can implement methods on Result<T, Error> instead of
// on Error directly. this makes life saner when doing ingest.
#[allow(unused)]
pub trait ErrorCtx<T>: Sized {
    fn with_field(self, field: &'static str) -> Result<T, ResourceError>;
    fn with_index(self, idx: usize) -> Result<T, ResourceError>;
    fn with_map_key(self, idx: String) -> Result<T, ResourceError>;

    #[allow(unused)]
    fn with_fields(self, a: &'static str, b: &'static str) -> Result<T, ResourceError> {
        self.with_field(b).with_field(a)
    }

    fn with_field_index(self, field: &'static str, index: usize) -> Result<T, ResourceError> {
        self.with_index(index).with_field(field)
    }

    fn with_field_key<F: std::fmt::Display>(
        self,
        field: &'static str,
        key: F,
    ) -> Result<T, ResourceError> {
        self.with_map_key(key.to_string()).with_field(field)
    }
}

impl<T, E> ErrorCtx<T> for Result<T, E>
where
    E: Into<ResourceError>,
{
    fn with_field(self, field: &'static str) -> Result<T, ResourceError> {
        match self {
            Ok(t) => Ok(t),
            Err(e) => Err(e.into().with_field(field)),
        }
    }

    fn with_index(self, idx: usize) -> Result<T, ResourceError> {
        match self {
            Ok(t) => Ok(t),
            Err(e) => Err(e.into().with_index(idx)),
        }
    }

    fn with_map_key(self, idx: String) -> Result<T, ResourceError> {
        match self {
            Ok(t) => Ok(t),
            Err(e) => Err(e.into().with_map_key(idx)),
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
        matches!(self, ResourceType::Cluster | ResourceType::Listener)
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

#[derive(Debug, Hash, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ResourceName {
    name: String,
}

impl std::fmt::Display for ResourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl ResourceName {
    pub fn wildcard() -> Self {
        Self {
            name: "*".to_string(),
        }
    }
}

impl AsRef<str> for ResourceName {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

impl From<&'static str> for ResourceName {
    fn from(name: &'static str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl From<String> for ResourceName {
    fn from(name: String) -> Self {
        Self { name }
    }
}

/// An xDS ConfigSource that specifies a resource is fetchable on the same ADS
/// connection as the current resource.
pub(super) const fn ads_config_source() -> xds_core::ConfigSource {
    xds_core::ConfigSource {
        config_source_specifier: Some(xds_core::config_source::ConfigSourceSpecifier::Ads(
            xds_core::AggregatedConfigSource {},
        )),
        resource_api_version: xds_core::ApiVersion::V3 as i32,
        authorities: Vec::new(),
        initial_fetch_timeout: None,
    }
}

/// Parse a u16 port from an xDS port specifier
#[inline]
pub(super) fn xds_port(
    port_specifier: Option<&xds_core::socket_address::PortSpecifier>,
) -> Option<u16> {
    match port_specifier {
        Some(xds_core::socket_address::PortSpecifier::PortValue(v)) => (*v).try_into().ok(),
        _ => None,
    }
}
