use crate::shared::{Attachment, SessionAffinityHashParam};
use crate::value_or_default;
#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use xds_api::pb::envoy::config::cluster::v3 as xds_cluster;
use xds_api::pb::envoy::config::cluster::v3::cluster::ring_hash_lb_config::HashFunction;

/// Policy for configuring a ketama-style consistent hashing algorithm.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RingHashParams {
    /// The minimum size of the hash ring
    #[serde(default = "default_min_ring_size", alias = "minRingSize")]
    pub min_ring_size: u32,

    /// How to hash an outgoing request into the ring.
    ///
    /// Hash parameters are applied in order. If the request is missing an
    /// input, it has no effect on the final hash. Hashing stops when only when
    /// all polcies have been applied or a `terminal` policy matches part of an
    /// incoming request.
    ///
    /// This allows configuring a fallback-style hash, where the value of
    /// `HeaderA` gets used, falling back to the value of `HeaderB`.
    ///
    /// If no policies match, a random hash is generated for each request.
    // FIXME: big question is still how to fill this field over xDS
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "hashParams")]
    pub hash_params: Vec<SessionAffinityHashParam>,
}

fn default_min_ring_size() -> u32 {
    1024
}

// TODO: figure out how we want to support the
// filter_state/connection_properties style of hashing based on source ip or
// grpc channel.
//
// TODO: add support for query parameter based hashing, which involves parsing
// query parameters, which http::uri just doesn't do. switch the whole crate to
// url::Url or something.
//
// TODO: Random, Maglev
//
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum LbPolicy {
    /// A simple round robin load balancing policy. Endpoints are picked in
    /// sequential order, but that order may vary client to client.
    #[default]
    RoundRobin,

    /// Use a ketama-style consistent hashing algorithm to route this request.
    RingHash(RingHashParams),

    /// No load balancing algorithm was specified.
    Unspecified,
}

impl LbPolicy {
    #[doc(hidden)]
    pub fn is_default_policy(&self) -> bool {
        match self {
            // FIXME: only Unspecified should be here
            LbPolicy::RoundRobin | LbPolicy::Unspecified => true,
            _ => false,
        }
    }

    //FIXME: work out what XDS leads to Unspecified being returned
    pub(crate) fn from_xds(cluster: &xds_cluster::Cluster) -> Option<Self> {
        match cluster.lb_policy() {
            // for ROUND_ROBIN, ignore the slow_start_config entirely and return
            // a brand new RoundRobin policy each time. validate that the config
            // matches the enum field even though it's ignored.
            xds_cluster::cluster::LbPolicy::RoundRobin => match cluster.lb_config.as_ref() {
                Some(xds_cluster::cluster::LbConfig::RoundRobinLbConfig(_)) | None => {
                    Some(LbPolicy::RoundRobin)
                }
                _ => None,
            },
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
                //FIXME(defaults): is this really the place to insist on defaults?
                let min_ring_size: u32 = value_or_default!(lb_config.minimum_ring_size, 2048)
                    .try_into()
                    .ok()?;

                let policy = RingHashParams {
                    min_ring_size,
                    hash_params: vec![], //FIXME(affinity): have to work out how to tunnel this over typed_extension_protocol_options
                };
                Some(LbPolicy::RingHash(policy))
            }
            _ => None,
        }
    }
}

//FIXME(persistence) enable session persistence as per the gateway API
// #[serde(default, skip_serializing_if = "Option::is_none")]
// pub session_persistence: Option<SessionPersistence>,
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Backend {
    pub attachment: Attachment,

    /// The route rules that determine whether any URLs match.
    pub lb: LbPolicy,
}

impl Backend {
    pub fn from_xds(xds: &xds_cluster::Cluster) -> Result<Self, crate::xds::Error> {
        let Some(lb) = LbPolicy::from_xds(xds) else {
            return Err(crate::xds::Error::InvalidXds {
                resource_type: "Cluster",
                resource_name: xds.name.clone(),
                message: "invalid LB config".to_string(),
            });
        };
        //FIXME(DNS): if discovery_type is logical dns, we should do this with dns
        Ok(Backend {
            attachment: Attachment::from_cluster_xds_name(&xds.name)?,
            lb,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_lb_policy() {
        let test_json = json!({
            "type":"RingHash",
            "minRingSize": 100
        });
        let obj: LbPolicy = serde_json::from_value(test_json.clone()).unwrap();

        assert_eq!(
            obj,
            LbPolicy::RingHash(RingHashParams {
                min_ring_size: 100,
                hash_params: vec![]
            })
        );

        assert_eq!(
            json!({"type": "RingHash", "min_ring_size": 100}),
            serde_json::to_value(obj).unwrap(),
        );
    }

    #[test]
    fn parses_backend() {
        let test_json = json!({
            "attachment": { "name": "foo", "namespace": "bar" },
            "lb": {
                "type":"Unspecified"
            }
        });
        let obj: Backend = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
