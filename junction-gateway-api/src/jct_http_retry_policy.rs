use crate::gateway_api::duration::Duration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use xds_api::pb::envoy::config::route::v3 as xds_route;

// informed by https://gateway-api.sigs.k8s.io/geps/gep-1731/
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JctHTTPRetryPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<Duration>,
}

impl JctHTTPRetryPolicy {
    pub fn from_xds(r: &xds_route::RetryPolicy) -> Self {
        let codes = r.retriable_status_codes.clone();
        let attempts = Some(r.num_retries.clone().map_or(1, |v| v.into()) as usize);
        let backoff = r
            .retry_back_off
            .as_ref()
            .map(|r2| r2.base_interval.clone().map(|x| x.try_into().unwrap()))
            .flatten();
        Self {
            codes,
            attempts,
            backoff,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::jct_http_retry_policy::JctHTTPRetryPolicy;

    #[test]
    fn parses_policy() {
        let test_json = serde_json::from_str::<serde_json::Value>(
            r#"{
            "codes":[ 1, 2 ],
            "attempts": 3,
            "backoff": "1m"
        }"#,
        )
        .unwrap();
        let obj: JctHTTPRetryPolicy = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
