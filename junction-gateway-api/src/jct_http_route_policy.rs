use crate::{
    jct_http_retry_policy::JctHTTPRetryPolicy,
    jct_http_session_affinity_policy::JctHTTPSessionAffinityPolicy, jct_parent_ref::JctParentRef,
};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    pub parent_ref: JctParentRef,

    #[serde(flatten)]
    pub inner: JctHTTPRoutePolicyConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct JctHTTPRoutePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity: Option<JctHTTPSessionAffinityPolicy>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<JctHTTPRetryPolicy>,
}

