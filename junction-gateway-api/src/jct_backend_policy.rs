use crate::{jct_lb_policy::JctLbPolicy, jct_parent_ref::JctParentRef};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema, Default)]
#[kube(
    group = "policies.junctionlabs.io",
    version = "v1",
    kind = "JctBackendPolicy",
    plural = "jctbackendpolicies"
)]
#[kube(namespaced)]
//#[kube(status = "JctHTTPRoutePolicyStatus")]
#[kube(derive = "Default")]
pub struct JctBackendPolicySpec {
    pub parent_ref: JctParentRef,

    #[serde(flatten)]
    pub inner: JctBackendPolicyConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
pub struct JctBackendPolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lb: Option<JctLbPolicy>,
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
