//! Backends are the logical target of network traffic. They have an identity and
//! a load-balancing policy. See [Backend] to get started.

use crate::{Error, Service};
use serde::{Deserialize, Serialize};

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

/// A Backend is uniquely identifiable by a combination of Service and port.
///
/// [Backend][crate::backend::Backend].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct BackendId {
    /// The logical traffic target that this backend configures.
    #[serde(flatten)]
    pub service: Service,

    /// The port backend traffic is sent on.
    pub port: u16,
}

impl std::fmt::Display for BackendId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write_name(f)
    }
}

impl std::str::FromStr for BackendId {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let (name, port) = super::parse_port(name)?;
        let port =
            port.ok_or_else(|| Error::new_static("expected a fully qualified name with a port"))?;
        let service = Service::from_str(name)?;

        Ok(Self { service, port })
    }
}

impl BackendId {
    /// The cannonical name of this ID. This is an alias for the
    /// [Display][std::fmt::Display] representation of this ID.
    pub fn name(&self) -> String {
        let mut buf = String::new();
        self.write_name(&mut buf).unwrap();
        buf
    }

    fn write_name(&self, w: &mut impl std::fmt::Write) -> std::fmt::Result {
        self.service.write_name(w)?;
        write!(w, ":{port}", port = self.port)?;

        Ok(())
    }

    #[doc(hidden)]
    pub fn lb_config_route_name(&self) -> String {
        let mut buf = String::new();
        self.write_lb_config_route_name(&mut buf).unwrap();
        buf
    }

    fn write_lb_config_route_name(&self, w: &mut impl std::fmt::Write) -> std::fmt::Result {
        self.service.write_lb_config_route_name(w)?;
        write!(w, ":{port}", port = self.port)?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn from_lb_config_route_name(name: &str) -> Result<Self, Error> {
        let (name, port) = super::parse_port(name)?;
        let port =
            port.ok_or_else(|| Error::new_static("expected a fully qualified name with a port"))?;

        let target = Service::from_lb_config_route_name(name)?;

        Ok(Self {
            service: target,
            port,
        })
    }
}

/// A Backend is a logical target for network traffic.
///
/// A backend configures how all traffic for its `target` is handled. Any
/// traffic routed to this backend will use the configured load balancing policy
/// to spread traffic across available endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Backend {
    /// A unique identifier for this backend.
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
    pub hash_params: Vec<RequestHashPolicy>,
}

pub(crate) const fn default_min_ring_size() -> u32 {
    1024
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RequestHashPolicy {
    /// Whether to stop immediately after hashing this value.
    ///
    /// This is useful if you want to try to hash a value, and then fall back to
    /// another as a default if it wasn't set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,

    #[serde(flatten)]
    pub hasher: RequestHasher,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum RequestHasher {
    /// Hash the value of a header. If the header has multiple values, they will
    /// all be used as hash input.
    #[serde(alias = "header")]
    Header {
        /// The name of the header to use as hash input.
        name: String,
    },

    /// Hash the value of an HTTP query parameter.
    #[serde(alias = "query")]
    QueryParam {
        /// The name of the query parameter to hash
        name: String,
    },
}

#[cfg(test)]
mod test {
    use std::fmt::Debug;

    use serde_json::json;

    use super::*;

    #[test]
    fn test_lb_policy_json() {
        assert_round_trip::<LbPolicy>(json!({
            "type":"Unspecified",
        }));
        assert_round_trip::<LbPolicy>(json!({
            "type":"RoundRobin",
        }));
        assert_round_trip::<LbPolicy>(json!({
            "type":"RingHash",
            "min_ring_size": 100,
            "hash_params": [
                {"type": "Header", "name": "x-user", "terminal": true},
                {"type": "QueryParam", "name": "u"},
            ]
        }));
    }

    #[test]
    fn test_backend_json() {
        assert_round_trip::<Backend>(json!({
            "id": {"type": "kube", "name": "foo", "namespace": "bar", "port": 789},
            "lb": {
                "type": "Unspecified",
            },
        }))
    }

    #[track_caller]
    fn assert_round_trip<T: Debug + Serialize + for<'a> Deserialize<'a>>(value: serde_json::Value) {
        let from_json: T = serde_json::from_value(value.clone()).expect("failed to deserialize");
        let round_tripped = serde_json::to_value(&from_json).expect("failed to serialize");

        assert_eq!(value, round_tripped, "serialized value should round-trip")
    }
}
