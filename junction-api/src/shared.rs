use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// PreciseHostname is the fully qualified domain name of a network host. This
/// matches the RFC 1123 definition of a hostname with 1 notable exception that
/// numeric IP addresses are not allowed.
///
/// Note that as per RFC1035 and RFC1123, a *label* must consist of lower case
/// alphanumeric characters or '-', and must start and end with an alphanumeric
/// character. No other punctuation is allowed.
pub type PreciseHostname = String;

/// PortNumber defines a network port.
pub type PortNumber = u16;

/// For directing to a k8s service (or a selected subset).
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
pub enum BackendKind {
    Service,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ParentRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
pub struct UnWeightedBackendRef {
    pub kind: BackendKind,

    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    // Port specifies the destination port number to use for this resource.
    /// Port is required when the referent is a Kubernetes Service or DNS name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
pub struct BackendRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,

    pub kind: BackendKind,

    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    // Port specifies the destination port number to use for this resource.
    /// Port is required when the referent is a Kubernetes Service or DNS name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
pub enum StringMatchType {
    #[default]
    Exact,
    RegularExpression,
}
