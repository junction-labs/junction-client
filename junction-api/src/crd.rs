///
/// These are the custom CRDs for junction-specific extensions that aren't
/// in the Gateway API CRDs.
///
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    backend::LbPolicy,
    http::{RouteRetryPolicy, SessionAffinityPolicy},
    shared::ParentRef,
};

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema, Default)]
#[kube(
    group = "policies.junctionlabs.io",
    version = "v1",
    kind = "JctHTTPRoutePolicy",
    plural = "jcthttproutepolicies"
)]
#[kube(namespaced)]
//#[kube(status = "JctHTTPRoutePolicyStatus")]
#[kube(derive = "Default")]
pub struct JctHTTPRoutePolicySpec {
    pub parent_ref: ParentRef,

    #[serde(flatten)]
    pub inner: JctHTTPRoutePolicyConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JctHTTPRoutePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity: Option<SessionAffinityPolicy>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RouteRetryPolicy>,
}

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema, Default)]
#[kube(
    group = "policies.junctionlabs.io",
    version = "v1",
    kind = "JctBackendPolicy",
    plural = "jctbackendpolicies"
)]
#[kube(namespaced)]
//#[kube(status = "JctBackendPolicyStatus")]
#[kube(derive = "Default")]
pub struct JctBackendPolicySpec {
    pub parent_ref: ParentRef,

    #[serde(flatten)]
    pub inner: JctBackendPolicyConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
pub struct JctBackendPolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lb: Option<LbPolicy>,
}

//FIXME: not needed till ezbake integrates
// impl JctBackendPolicyConfig {
//     pub fn from_annotations(annotations: &BTreeMap<String, String>) -> Option<Self> {
//         // after slash has to be 63 chars, alphanumeric, _, -, .
//         // XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX/012345678901234567890123456789012345678901234567890123456789012
//         // backend.policies.junctionlabs.io/lb.type = "RoundRobin"
//         // backend.policies.junctionlabs.io/lb.minRingSize = 3
//         todo!();
//     }
// }
