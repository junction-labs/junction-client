//! Backends are the logical target of network traffic. They have an identity and
//! a load-balancing policy. See [Backend] to get started.

use crate::BackendId;
use serde::{Deserialize, Serialize};

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

/// A Backend is a logical target for network traffic.
///
/// A backend configures how all traffic for it's `target` is handled. Any
/// traffic routed to this backend will use its load balancing policy to evenly
/// spread traffic across all available endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Backend {
    /// A unique description of what this backend is.
    pub id: BackendId,

    /// How traffic to this target should be load balanced.
    pub lb: LbPolicy,
}

// TODO: figure out how we want to support the filter_state/connection_properties style of hashing
// based on source ip or grpc channel.
//
// TODO: add support for query parameter based hashing, which involves parsing query parameters,
// which http::uri just doesn't do. switch the whole crate to url::Url or something.
//
// TODO: Random, Maglev
//
/// A policy describing how traffic to this target should be load balanced.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum LbPolicy {
    /// A simple round robin load balancing policy. Endpoints are picked in sequential order, but
    /// that order may vary client to client.
    RoundRobin,

    /// Use a ketama-style consistent hashing algorithm to route this request.
    RingHash(RingHashParams),

    /// No load balancing algorithm was specified. Clients may decide how load balancing happens
    /// for this target.
    #[default]
    Unspecified,
}

impl LbPolicy {
    /// Return `true` if this policy is [LbPolicy::Unspecified].
    pub fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }
}

/// Policy for configuring a ketama-style consistent hashing algorithm.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RingHashParams {
    /// The minimum size of the hash ring
    #[serde(default = "default_min_ring_size", alias = "minRingSize")]
    pub min_ring_size: u32,

    /// How to hash an outgoing request into the ring.
    ///
    /// Hash parameters are applied in order. If the request is missing an input, it has no effect
    /// on the final hash. Hashing stops when only when all polices have been applied or a
    /// `terminal` policy matches part of an incoming request.
    ///
    /// This allows configuring a fallback-style hash, where the value of `HeaderA` gets used,
    /// falling back to the value of `HeaderB`.
    ///
    /// If no policies match, a random hash is generated for each request.
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "hashParams")]
    pub hash_params: Vec<SessionAffinityHashParam>,
}

pub(crate) const fn default_min_ring_size() -> u32 {
    1024
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
    fn test_backend_json_roundtrip() {
        let test_json = json!({
            "id": { "name": "foo", "namespace": "bar", "port": 789 },
            "lb": {
                "type": "Unspecified",
            },
        });
        let obj: Backend = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum SessionAffinityHashParamType {
    /// Hash the value of a header. If the header has multiple values, they will all be used as hash
    /// input.
    #[serde(alias = "header")]
    Header {
        /// The name of the header to use as hash input.
        name: String,
    },
}

// FIXME: Ben votes to skip the extra "affinity" naming here as its redundant
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct SessionAffinityHashParam {
    /// Whether to stop immediately after hashing this value.
    ///
    /// This is useful if you want to try to hash a value, and then fall back to another as a
    /// default if it wasn't set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,

    #[serde(flatten)]
    pub matcher: SessionAffinityHashParamType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct SessionAffinity {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        alias = "hashParams",
        alias = "HashParams"
    )]
    pub hash_params: Vec<SessionAffinityHashParam>,
}

#[cfg(test)]
mod test_session_affinity {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_session_affinity_policy() {
        let test_json = json!({
            "hash_params": [
                { "type": "Header", "name": "FOO",  "terminal": true },
                { "type": "Header", "name": "FOO"}
            ]
        });
        let obj: SessionAffinity = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
