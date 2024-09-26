use crate::shared::{Attachment, SessionAffinityHashParam};
use crate::value_or_default;
#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use xds_api::pb::envoy::config::cluster::v3 as xds_cluster;
use xds_api::pb::envoy::config::cluster::v3::cluster::ring_hash_lb_config::HashFunction;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RingHashParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ring_size: Option<u32>,

    // FIXME: Ben votes to skip the extra "affinity" naming here as its redundant
    // that is fine, big question is still how to fill this field over xDS
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hash_params: Vec<SessionAffinityHashParam>,
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
    #[default]
    RoundRobin,
    RingHash(RingHashParams),
    Unspecified,
}

impl LbPolicy {
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
                    min_ring_size: Some(min_ring_size),
                    hash_params: vec![], //FIXME(affinity): have to work out how ro tunnel this over typed_extension_protocol_options
                };
                Some(LbPolicy::RingHash(policy))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Backend {
    pub attachment: Attachment,

    /// The route rules that determine whether any URLs match.
    pub lb: LbPolicy,
    //FIXME(persistence) enable session persistence as per the gateway API
    // #[serde(default, skip_serializing_if = "Option::is_none")]
    // pub session_persistence: Option<SessionPersistence>,
}

fn cluster_discovery_type(
    cluster: &xds_cluster::Cluster,
) -> Option<xds_cluster::cluster::DiscoveryType> {
    match cluster.cluster_discovery_type {
        Some(xds_cluster::cluster::ClusterDiscoveryType::Type(cdt)) => {
            xds_cluster::cluster::DiscoveryType::try_from(cdt).ok()
        }
        _ => None,
    }
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
        let Some(discovery_type) = cluster_discovery_type(&xds) else {
            return Err(crate::xds::Error::InvalidXds {
                resource_type: "Cluster",
                resource_name: xds.name.clone(),
                message: format!("invalid discovery_type: {:?}", xds.cluster_discovery_type),
            });
        };

        let name = match discovery_type {
            xds_cluster::cluster::DiscoveryType::Eds => {
                let Some(eds_config) = xds.eds_cluster_config.as_ref() else {
                    return Err(crate::xds::Error::InvalidXds {
                        resource_type: "Cluster",
                        resource_name: xds.name.clone(),
                        message: "an EDS cluster must have an eds_cluster_config".to_string(),
                    });
                };

                if !eds_config.service_name.is_empty() {
                    eds_config.service_name.clone()
                } else {
                    xds.name.clone()
                }
            }
            _ => {
                return Err(crate::xds::Error::InvalidXds {
                    resource_type: "Cluster",
                    resource_name: xds.name.clone(),
                    message: "only EDS clusters are supported".to_string(),
                })
            }
        };

        //FIXME(DNS): if discovery_type is logical dns, we should do this with dns
        Ok(Backend {
            attachment: Attachment::from_cluster_xds_name(&name)?,
            lb,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::backend::{Backend, LbPolicy};

    #[test]
    fn parses_lb_policy() {
        let test_json = json!({
            "type":"RingHash",
            "minRingSize": 100
        });
        let obj: LbPolicy = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
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
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
