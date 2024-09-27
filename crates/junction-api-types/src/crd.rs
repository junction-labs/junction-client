use crate::{backend::LbPolicy, http::RouteRetryPolicy, shared::SessionAffinityPolicy};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

///
/// These are the custom CRDs for junction-specific extensions that aren't
/// in the Gateway API CRDs.
///

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocalPolicyTargetReference {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    pub name: String,
}

///
///  HTTPRoutePolicyConfig is due to become in the standard, and we have picked
///  the exact parameters likely to standardize. However (a) they may change and
///  (b) aren't in any CRDs yet. So this policy is to allow those, and any
///  similar extensions in the future.
///
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
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
    pub target_refs: Vec<LocalPolicyTargetReference>,

    #[serde(flatten)]
    pub inner: JctHTTPRoutePolicyConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JctHTTPRoutePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity: Option<SessionAffinityPolicy>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RouteRetryPolicy>,
}

///
/// session_persitence will be handled the the upcoming 1.2 release of BackendLBPolicy
/// so the junction CRD only has to support our customized load balancer config.
///
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
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
    pub target_refs: Vec<LocalPolicyTargetReference>,

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
