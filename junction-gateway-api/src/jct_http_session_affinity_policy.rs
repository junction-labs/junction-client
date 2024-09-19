use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use xds_api::pb::envoy::config::route::v3 as xds_route;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum JctHTTPSessionAffinityHashParamType {
    Header,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JctHTTPSessionAffinityHashParam {
    pub r#type: JctHTTPSessionAffinityHashParamType,
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JctHTTPSessionAffinityPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hash_params: Vec<JctHTTPSessionAffinityHashParam>,
}

impl JctHTTPSessionAffinityHashParam {
    //only returns session affinity
    pub fn from_xds(hash_policy: &xds_route::route_action::HashPolicy) -> Option<Self> {
        use xds_route::route_action::hash_policy::PolicySpecifier;

        match hash_policy.policy_specifier.as_ref() {
            Some(PolicySpecifier::Header(h)) => Some(JctHTTPSessionAffinityHashParam {
                terminal: hash_policy.terminal,
                r#type: JctHTTPSessionAffinityHashParamType::Header,
                name: h.header_name.clone(),
            }),
            _ => {
                //FIXME; thrown away config
                None
            }
        }
    }
}

impl JctHTTPSessionAffinityPolicy {
    //only returns session affinity
    pub fn from_xds(hash_policy: &Vec<xds_route::route_action::HashPolicy>) -> Option<Self> {
        let hash_params: Vec<_> = hash_policy
            .iter()
            .filter_map(JctHTTPSessionAffinityHashParam::from_xds)
            .collect();

        if hash_params.len() == 0 {
            return None;
        } else {
            return Some(JctHTTPSessionAffinityPolicy { hash_params });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::jct_http_session_affinity_policy::JctHTTPSessionAffinityPolicy;

    #[test]
    fn parses_policy() {
        let test_json = serde_json::from_str::<serde_json::Value>(
            r#"{
            "hashParams": [
                { "type": "Header", "name": "FOO",  "terminal": true },
                { "type": "Header", "name": "FOO"}
            ]
        }"#,
        )
        .unwrap();
        let obj: JctHTTPSessionAffinityPolicy = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
