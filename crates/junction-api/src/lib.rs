//! The Junction configuration API. This crate allows you to build configuration
//! for a dynamic HTTP client and export it to a control plane or pass it
//! directly to an in-process client. Junction configuration is expressible as
//! plain Rust structs, and can be serialized/deserialized with a [serde]
//! compatible library.
//!
//! # Core Concepts
//!
//! ## Service
//!
//! The Junction API is built around the idea that you're always routing
//! requests to a [Service], which is an abstract representation of a place you
//! might want traffic to go. A [Service] can be anything, but to use one in
//! Junction you need a way to uniquely specify it. That could be anything from
//! a DNS name someone else has already set up to a Kubernetes Service in a
//! cluster you've connected to Junction.
//!
//! ## Routes
//!
//! An HTTP [Route][crate::http::Route] is the client facing half of Junction,
//! and contains most of the things you'd traditionally find in a hand-rolled
//! HTTP client - timeouts, retries, URL rewriting and more. Routes match
//! outgoing requests based on their method, URL, and headers. The [http]
//! module's documentation goes into detail on how and why to configure a Route.
//!
//! ## Backends
//!
//! A [Backend][crate::backend::Backend] is a single port on a Service. Backends
//! configuration gives you control over the things you'd normally configure in
//! a reverse proxy or a traditional load balancer. See the [backend] module's
//! documentation for more detail.
//!
//! # Crate Feature Flags
//!
//! The following feature flags are available:
//!
//! * The `kube` feature includes conversions from Junction configuration to and
//!   from Kubernetes objects. This feature depends on the `kube` and
//!   `k8s-openapi` crates. See the [kube] module docs for more detail.
//!
//! * The `xds` feature includes conversions from Junction configuration to and
//!   from xDS types. This feature depends on the [xds-api][xds_api] crate.

mod error;
use std::{ops::Deref, str::FromStr};

use backend::BackendId;
pub use error::Error;

pub mod backend;
pub mod http;

mod shared;
pub use shared::{Duration, Fraction, Regex};

#[cfg(feature = "xds")]
mod xds;

#[cfg(any(feature = "kube_v1_29", feature = "kube_v1_30", feature = "kube_v1_31"))]
pub mod kube;

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

use serde::{de::Visitor, Deserialize, Serialize};

#[cfg(feature = "xds")]
macro_rules! value_or_default {
    ($value:expr, $default:expr) => {
        $value.as_ref().map(|v| v.value).unwrap_or($default)
    };
}

use smol_str::ToSmolStr;
#[cfg(feature = "xds")]
pub(crate) use value_or_default;

// TODO: replace String with SmolStr

macro_rules! newtype_string {
    ($(#[$id_attr:meta])* pub $name:ident) => {
        $(#[$id_attr])*
        #[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(smol_str::SmolStr);

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{:?}", self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                // implements both visit_str and visit_string in case that moving the
                // string into this $name instead of cloning is possible.
                struct NameVisitor;

                impl<'de> Visitor<'de> for NameVisitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                        formatter.write_str("a valid DNS $name")
                    }

                    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        let name: Result<$name, Error> = v.try_into();
                        name.map_err(|e: Error| E::custom(e.to_string()))
                    }

                    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        $name::from_str(v).map_err(|e| E::custom(e.to_string()))
                    }
                }

                deserializer.deserialize_string(NameVisitor)
            }
        }

        #[cfg(feature = "typeinfo")]
        impl junction_typeinfo::TypeInfo for $name {
            fn kind() -> junction_typeinfo::Kind {
                junction_typeinfo::Kind::String
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::validate(s.as_bytes())?;
                Ok($name(smol_str::SmolStr::new(s)))
            }
        }

        impl TryFrom<String> for $name {
            type Error = Error;

            fn try_from(value: String) -> Result<$name, Self::Error> {
                $name::validate(value.as_bytes())?;
                Ok($name(smol_str::SmolStr::new(value)))
            }
        }

        impl<'a> TryFrom<&'a str> for $name {
            type Error = Error;

            fn try_from(value: &'a str) -> Result<$name, Self::Error> {
                $name::validate(value.as_bytes())?;
                Ok($name(smol_str::SmolStr::new(value)))
            }
        }
    }
}

// A lookup table of valid RFC 1123 name characters. RFC 1035 labels use
// this table as well but ignore the '.' character.
//
// Adapted from the table used in the http crate to valid URI and Authority
// strings.
//
// https://github.com/hyperium/http/blob/master/src/uri/mod.rs#L146-L153
#[rustfmt::skip]
const DNS_CHARS: [u8; 256] = [
//  0      1      2      3      4      5      6      7      8      9
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, //   x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, //  1x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, //  2x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, //  3x
    0,     0,     0,     0,     0,  b'-',  b'.',     0,  b'0',  b'1', //  4x
 b'2',  b'3',  b'4',  b'5',  b'6',  b'7',  b'8',  b'9',     0,     0, //  5x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, //  6x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, //  7x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, //  8x
    0,     0,     0,     0,     0,     0,     0,  b'a',  b'b',  b'c', //  9x
 b'd',  b'e',  b'f',  b'g',  b'h',  b'i',  b'j',  b'k',  b'l',  b'm', // 10x
 b'n',  b'o',  b'p',  b'q',  b'r',  b's',  b't',  b'u',  b'v',  b'w', // 11x
 b'x',  b'y',  b'z',     0,     0,     0,     0,     0,     0,     0, // 12x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 13x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 14x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 15x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 16x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 17x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 18x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 19x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 20x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 21x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 22x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 23x
    0,     0,     0,     0,     0,     0,     0,     0,     0,     0, // 24x
    0,     0,     0,     0,     0,     0,                              // 25x
];

newtype_string! {
    /// AN RFC 1123 DNS domain name.
    ///
    /// This name must be no more than 253 characters, and can only contain
    /// lowercase ascii alphanumeric characters, `.` and `-`.
    pub Hostname
}

impl Hostname {
    /// The RFC 1035 max length. We don't add any extra validation to it, since
    /// there's a variety of places Name gets used and they won't all have the
    /// same constraints.
    const MAX_LEN: usize = 253;

    /// Create a new hostname from a static string.
    ///
    /// Assumes that a human being has manually validated that this is a valid
    /// hostname and will panic if it is not.
    pub fn from_static(src: &'static str) -> Self {
        Self::validate(src.as_bytes()).expect("expected a static Name to be valid");
        Self(smol_str::SmolStr::new_static(src))
    }

    fn validate(bs: &[u8]) -> Result<(), Error> {
        if bs.is_empty() {
            return Err(Error::new_static("Hostname must not be empty"));
        }

        if bs.len() > Self::MAX_LEN {
            return Err(Error::new_static(
                "Hostname must not be longer than 253 characters",
            ));
        }

        for (i, &b) in bs.iter().enumerate() {
            match (i, DNS_CHARS[b as usize]) {
                (_, 0) => return Err(Error::new_static("Hostname contains an invalid character")),
                (0, b'-' | b'.') => {
                    return Err(Error::new_static(
                        "Hostname must start with an alphanumeric character",
                    ))
                }
                (i, b'-' | b'.') if i == bs.len() => {
                    return Err(Error::new_static(
                        "Hostname must end with an alphanumeric character",
                    ))
                }
                _ => (),
            }
        }

        Ok(())
    }
}

newtype_string! {
    /// An RFC 1035 compatible name. This name must be useable as a component of a
    /// DNS subdomain - it must start with a lowercase ascii alphabetic character
    /// and may only consist of ascii lowercase alphanumeric characters and the `-`
    /// character.
    pub Name
}

impl Name {
    // RFC 1035 max length.
    const MAX_LEN: usize = 63;

    /// Create a new name from a static string.
    ///
    /// Assumes that a human being has manually validated that this is a valid name
    /// and will panic if it is not.
    pub fn from_static(src: &'static str) -> Self {
        Self::validate(src.as_bytes()).expect("expected a static Name to be valid");
        Self(smol_str::SmolStr::new_static(src))
    }

    /// Check that a `str` is a valid Name.
    ///
    /// Being a valid name also implies that the slice is valid utf-8.
    fn validate(bs: &[u8]) -> Result<(), Error> {
        if bs.is_empty() {
            return Err(Error::new_static("Name must not be empty"));
        }

        if bs.len() > Self::MAX_LEN {
            return Err(Error::new_static(
                "Name must not be longer than 63 characters",
            ));
        }

        for (i, &b) in bs.iter().enumerate() {
            match (i, DNS_CHARS[b as usize]) {
                (_, 0 | b'.') => {
                    return Err(Error::new_static("Name contains an invalid character"))
                }
                (0, b'-' | b'0'..=b'9') => {
                    return Err(Error::new_static("Name must start with [a-z]"))
                }
                (i, b'-') if i == bs.len() => {
                    return Err(Error::new_static(
                        "Name must end with an alphanumeric character",
                    ))
                }
                _ => (),
            }
        }

        Ok(())
    }
}

/// A uniquely identifiable service that traffic can be routed to.
///
/// Services are abstract, and can have different semantics depending on where
/// and how they're defined.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum Service {
    /// A DNS hostname.
    ///
    /// See [DnsService] for details.
    #[serde(rename = "dns", alias = "DNS")]
    Dns(DnsService),

    /// A Kubernetes Service.
    ///
    /// See [KubeService] for details.
    #[serde(rename = "kube", alias = "k8s")]
    Kube(KubeService),
}

impl std::fmt::Display for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write_name(f)
    }
}

impl std::str::FromStr for Service {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let hostname = Hostname::from_str(name)?;

        let target = match name {
            n if n.ends_with(KubeService::SUBDOMAIN) => {
                let parts: Vec<_> = hostname.split('.').collect();
                if parts.len() != 5 {
                    return Err(Error::new_static(
                        "invalid Service target: name and namespace must be valid DNS labels",
                    ));
                }

                let name = parts[0].parse()?;
                let namespace = parts[1].parse()?;
                Service::Kube(KubeService { name, namespace })
            }
            _ => Service::Dns(DnsService { hostname }),
        };
        Ok(target)
    }
}

impl Service {
    /// Create a new DNS target. The given name must be a valid RFC 1123 DNS
    /// hostname.
    pub fn dns(name: &str) -> Result<Self, Error> {
        let hostname = Hostname::from_str(name)?;
        Ok(Self::Dns(DnsService { hostname }))
    }

    /// Create a new Kubernetes Service target.
    ///
    /// `name` and `hostname` must be valid DNS subdomain labels.
    pub fn kube(namespace: &str, name: &str) -> Result<Self, Error> {
        let namespace = Name::from_str(namespace)?;
        let name = Name::from_str(name)?;

        Ok(Self::Kube(KubeService { name, namespace }))
    }

    /// Clone and convert this backend into a [BackendId] with the specified port.
    pub fn as_backend_id(&self, port: u16) -> BackendId {
        BackendId {
            service: self.clone(),
            port,
        }
    }

    /// The canonical hostname for this backend.
    pub fn hostname(&self) -> Hostname {
        match self {
            Service::Dns(dns) => dns.hostname.clone(),
            _ => {
                // safety: a name should never be an invalid hostname
                let name = self.name().to_smolstr();
                Hostname(name)
            }
        }
    }

    /// The canonical name of this backend.
    ///
    /// Returns the same name as [hostname][Self::hostname] but as a raw
    /// [String] instead of a [Hostname].
    pub fn name(&self) -> String {
        let mut buf = String::new();
        self.write_name(&mut buf).unwrap();
        buf
    }

    fn write_name(&self, w: &mut impl std::fmt::Write) -> std::fmt::Result {
        match self {
            Service::Dns(dns) => {
                w.write_str(&dns.hostname)?;
            }
            Service::Kube(svc) => {
                write!(
                    w,
                    "{name}.{namespace}{subdomain}",
                    name = svc.name,
                    namespace = svc.namespace,
                    subdomain = KubeService::SUBDOMAIN,
                )?;
            }
        }

        Ok(())
    }

    const BACKEND_SUBDOMAIN: &'static str = ".lb.jct";

    #[doc(hidden)]
    pub fn lb_config_route_name(&self) -> String {
        let mut buf = String::new();
        self.write_lb_config_route_name(&mut buf).unwrap();
        buf
    }

    fn write_lb_config_route_name(&self, w: &mut impl std::fmt::Write) -> std::fmt::Result {
        match self {
            Service::Dns(dns) => {
                write!(w, "{}{}", dns.hostname, Service::BACKEND_SUBDOMAIN)?;
            }
            Service::Kube(svc) => {
                write!(
                    w,
                    "{name}.{namespace}{svc}{backend}",
                    name = svc.name,
                    namespace = svc.namespace,
                    svc = KubeService::SUBDOMAIN,
                    backend = Service::BACKEND_SUBDOMAIN,
                )?;
            }
        }

        Ok(())
    }

    #[doc(hidden)]
    pub fn from_lb_config_route_name(name: &str) -> Result<Self, Error> {
        let hostname = Hostname::from_str(name)?;

        let Some(hostname) = hostname.strip_suffix(Service::BACKEND_SUBDOMAIN) else {
            return Err(Error::new_static("expected a Junction backend name"));
        };

        Self::from_str(hostname)
    }
}

/// A Kubernetes Service to target with traffic.
///
/// This Service doesn't have to exist in any particular Kubernetes cluster -
/// Junction assumes that all connected Kubernetes clusters have adopted
/// [namespace-sameness] instead of asking you to distinguish between individual
/// clusters as they come and go.
///
/// [namespace-sameness]: https://github.com/kubernetes/community/blob/master/sig-multicluster/namespace-sameness-position-statement.md
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct KubeService {
    /// The name of the Kubernetes Service to target.
    pub name: Name,

    /// The namespace of the Kubernetes service to target. This must be explicitly
    /// specified, and won't be inferred from context.
    pub namespace: Name,
}

impl KubeService {
    const SUBDOMAIN: &'static str = ".svc.cluster.local";
}

/// A DNS name to target with traffic.
///
/// While the whole point of Junction is to avoid DNS in the first place,
/// sometimes you have to route traffic to an external service or something that
/// can't (or shouldn't) be connected directly to this instance of Junction. In
/// those cases, it's useful to treat a DNS name like a slowly changing pool of
/// addresses and to route traffic to them appropriately.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct DnsService {
    /// A valid RFC1123 DNS domain name.
    pub hostname: Hostname,
}

#[inline]
pub(crate) fn parse_port(s: &str) -> Result<(&str, Option<u16>), Error> {
    let (name, port) = match s.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (s, None),
    };

    let port = match port {
        Some(port) => {
            Some(u16::from_str(port).map_err(|_| Error::new_static("invalid port number"))?)
        }
        None => None,
    };

    Ok((name, port))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_service_serde() {
        let target = serde_json::json!({
            "name": "foo",
            "namespace": "potato",
            "type": "kube",
        });

        assert_eq!(
            serde_json::from_value::<Service>(target).unwrap(),
            Service::kube("potato", "foo").unwrap(),
        );

        let target = serde_json::json!({
            "type": "dns",
            "hostname": "example.com"
        });

        assert_eq!(
            serde_json::from_value::<Service>(target).unwrap(),
            Service::dns("example.com").unwrap(),
        );
    }

    #[test]
    fn test_service_name() {
        assert_name(
            Service::kube("production", "potato").unwrap(),
            "potato.production.svc.cluster.local",
        );

        assert_name(
            Service::dns("cool-stuff.example.com").unwrap(),
            "cool-stuff.example.com",
        );
    }

    #[track_caller]
    fn assert_name(target: Service, str: &'static str) {
        assert_eq!(&target.name(), str);
        let parsed = Service::from_str(str).unwrap();
        assert_eq!(parsed, target);
    }

    #[test]
    fn test_target_lb_config_name() {
        assert_lb_config_name(
            Service::kube("production", "potato").unwrap(),
            "potato.production.svc.cluster.local.lb.jct",
        );

        assert_lb_config_name(
            Service::dns("cool-stuff.example.com").unwrap(),
            "cool-stuff.example.com.lb.jct",
        );
    }

    #[track_caller]
    fn assert_lb_config_name(target: Service, str: &'static str) {
        assert_eq!(&target.lb_config_route_name(), str);
        let parsed = Service::from_lb_config_route_name(str).unwrap();
        assert_eq!(parsed, target);
    }

    #[test]
    fn test_valid_name() {
        let alphabetic: Vec<char> = (b'a'..=b'z').map(|b| b as char).collect();
        let full_alphabet: Vec<char> = (b'a'..=b'z')
            .chain(b'0'..=b'9')
            .chain([b'-'])
            .map(|b| b as char)
            .collect();

        arbtest::arbtest(|u| {
            let input = arbitrary_string(u, &alphabetic, &full_alphabet, Name::MAX_LEN);
            let res = Name::from_str(&input);

            assert!(
                res.is_ok(),
                "string should be a valid name: {input:?} (len={}): {}",
                input.len(),
                res.unwrap_err(),
            );
            Ok(())
        });
    }

    #[test]
    fn test_valid_hostname() {
        let alphanumeric: Vec<char> = (b'a'..=b'z')
            .chain(b'0'..=b'9')
            .map(|b| b as char)
            .collect();
        let full_alphabet: Vec<char> = (b'a'..=b'z')
            .chain(b'0'..=b'9')
            .chain([b'-', b'.'])
            .map(|b| b as char)
            .collect();

        arbtest::arbtest(|u| {
            let input = arbitrary_string(u, &alphanumeric, &full_alphabet, Hostname::MAX_LEN);
            let res = Hostname::from_str(&input);
            assert!(
                res.is_ok(),
                "string should be a valid hostname: {input:?} (len={}): {}",
                input.len(),
                res.unwrap_err(),
            );
            Ok(())
        });
    }

    fn arbitrary_string(
        u: &mut arbitrary::Unstructured,
        first_and_last: &[char],
        alphabet: &[char],
        max_len: usize,
    ) -> String {
        let len: usize = u.choose_index(max_len - 1).unwrap() + 1;
        let mut input = String::new();

        if len > 0 {
            input.push(*u.choose(first_and_last).unwrap());
        }

        if len > 1 {
            for _ in 1..(len - 1) {
                input.push(*u.choose(alphabet).unwrap());
            }
        }

        if len > 2 {
            input.push(*u.choose(first_and_last).unwrap());
        }

        input
    }
}
