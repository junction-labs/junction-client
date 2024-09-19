use crate::value_or_default;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use xds_api::pb::envoy::config::cluster::v3 as xds_cluster;
use xds_api::pb::envoy::config::cluster::v3::cluster::ring_hash_lb_config::HashFunction;

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JctRingHashParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ring_size: Option<usize>,
}

// TODO: figure out how we want to support the filter_state/connection_properties
// style of hashing based on source ip or grpc channel.
//
// TODO: add support for query parameter based hashing, which involves parsing
// query parameters, which http::uri just doesn't do. switch the whole crate to
// url::Url or something.
//
// TODO: Random, Maglev
//
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum JctLbPolicy {
    #[default]
    RoundRobin,
    RingHash(JctRingHashParams),
}

impl JctLbPolicy {
    pub fn from_xds(cluster: &xds_cluster::Cluster) -> Option<Self> {
        match cluster.lb_policy() {
            // for ROUND_ROBIN, ignore the slow_start_config entirely and return
            // a brand new RoundRobin policy each time. validate that the config
            // matches the enum field even though it's ignored.
            xds_cluster::cluster::LbPolicy::RoundRobin => {
                match cluster.lb_config.as_ref() {
                    Some(xds_cluster::cluster::LbConfig::RoundRobinLbConfig(_)) | None => (),
                    _ => return None,
                };

                Some(JctLbPolicy::RoundRobin)
            }
            // for RING_HASH pull the config out if set or use default values to
            // populate our config.
            xds_cluster::cluster::LbPolicy::RingHash => {
                let lb_config = match cluster.lb_config.as_ref() {
                    Some(xds_cluster::cluster::LbConfig::RingHashLbConfig(config)) => config,
                    None => &xds_cluster::cluster::RingHashLbConfig::default(),
                    _ => return None,
                };

                // hash function must be XX_HASH to match gRPC
                if lb_config.hash_function() != HashFunction::XxHash {
                    return None;
                }
                //fixme: is this really the place to insist on defaults?
                let min_ring_size: usize = value_or_default!(lb_config.minimum_ring_size, 2048)
                    .try_into()
                    .ok()?;

                let policy = JctRingHashParams {
                    min_ring_size: Some(min_ring_size),
                };
                Some(JctLbPolicy::RingHash(policy))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::jct_lb_policy::JctLbPolicy;

    #[test]
    fn parses_policy() {
        let test_json = serde_json::from_str::<serde_json::Value>(
            r#"{
            "type":"RingHash",
            "minRingSize": 100
        }"#,
        )
        .unwrap();
        let obj: JctLbPolicy = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
