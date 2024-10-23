//! Junction API Configuration.
//!
//! These types let you express routing and load balancing configuration as data
//! structures. The `kube` and `xds` features of this crate let you convert
//! Junction configuration to Kubernetes objects and xDS resources respectively.
//!
//! Use this crate directly if you're looking to build and export configuration.
//! Use the `junction-client` crate if you want to make requests and resolve
//! addresses with Junction.

mod error;
use std::{ops::Deref, str::FromStr};

pub use error::Error;

pub mod backend;
pub mod http;

mod shared;
pub use shared::{Duration, Fraction, Regex};

#[cfg(feature = "xds")]
mod xds;

#[cfg(feature = "kube")]
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

#[cfg(feature = "xds")]
pub(crate) use value_or_default;

macro_rules! newtype_string {
    ($(#[$id_attr:meta])* pub $name:ident) => {
        $(#[$id_attr])*
        #[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

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
                Ok($name(s.to_string()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = Error;

            fn try_from(value: String) -> Result<$name, Self::Error> {
                $name::validate(value.as_bytes())?;
                Ok($name(value))
            }
        }

        impl<'a> TryFrom<&'a str> for $name {
            type Error = Error;

            fn try_from(value: &'a str) -> Result<$name, Self::Error> {
                $name::validate(value.as_bytes())?;
                Ok($name(value.to_string()))
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
    /// lowercase ascii alphanumeric charaters, `.` and `-`.
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
        Self(src.to_string())
    }

    fn validate(bs: &[u8]) -> Result<(), Error> {
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
        Self(src.to_string())
    }

    /// Check that a `str` is a valid Name.
    ///
    /// Being a valid name also implies that the slice is valid utf-8.
    fn validate(bs: &[u8]) -> Result<(), Error> {
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct ServiceTarget {
    /// The name of the Kubernetes Service to target.
    pub name: Name,

    /// The namespace of the Kubernetes service to target. This must be explicitly
    /// specified, and won't be inferred from context.
    pub namespace: Name,

    /// The port number of the Kubernetes service to target/ attach to.
    ///
    /// When attaching policies, if it is not specified, the target will apply
    /// to all connections that don't have a specific port specified.
    ///
    /// When being used to lookup a backend after a matched rule, if it is not
    /// specified then it will use the same port as the incoming request
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl ServiceTarget {
    const SUBDOMAIN: &'static str = ".svc.cluster.local";
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct DNSTarget {
    /// The DNS name to target.
    pub hostname: Hostname,

    /// The port number to target/attach to.
    ///
    /// When attaching policies, if it is not specified, the target will apply
    /// to all connections that don't have a specific port specified.
    ///
    /// When being used to lookup a backend after a matched rule, if it is not
    /// specified then it will use the same port as the incoming request
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum Target {
    #[serde(alias = "dns")]
    DNS(DNSTarget),

    #[serde(untagged)]
    Service(ServiceTarget),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write_name(f)
    }
}

impl Target {
    const BACKEND_SUBDOMAIN: &'static str = ".lb.jct";

    pub fn name(&self) -> String {
        let mut buf = String::new();
        self.write_name(&mut buf).unwrap();
        buf
    }

    fn write_name(&self, w: &mut impl std::fmt::Write) -> std::fmt::Result {
        match self {
            Target::DNS(dns) => {
                w.write_str(&dns.hostname)?;

                if let Some(port) = dns.port {
                    write!(w, ":{port}")?;
                }
            }
            Target::Service(svc) => {
                write!(
                    w,
                    "{name}.{namespace}{subdomain}",
                    name = svc.name,
                    namespace = svc.namespace,
                    subdomain = ServiceTarget::SUBDOMAIN,
                )?;

                if let Some(port) = svc.port {
                    write!(w, ":{port}")?;
                }
            }
        }

        Ok(())
    }

    #[doc(hidden)]
    pub fn passthrough_route_name(&self) -> String {
        let mut buf = String::new();
        self.write_passthrough_route_name(&mut buf).unwrap();
        buf
    }

    fn write_passthrough_route_name(&self, w: &mut impl std::fmt::Write) -> std::fmt::Result {
        match self {
            Target::DNS(dns) => {
                write!(w, "{}{}", dns.hostname, Target::BACKEND_SUBDOMAIN)?;

                if let Some(port) = dns.port {
                    write!(w, ":{port}")?;
                }
            }
            Target::Service(svc) => {
                write!(
                    w,
                    "{name}.{namespace}{svc}{backend}",
                    name = svc.name,
                    namespace = svc.namespace,
                    svc = ServiceTarget::SUBDOMAIN,
                    backend = Target::BACKEND_SUBDOMAIN,
                )?;

                if let Some(port) = svc.port {
                    write!(w, ":{port}")?;
                }
            }
        }

        Ok(())
    }

    pub fn from_name(name: &str) -> Result<Self, Error> {
        let (name, port) = parse_port(name)?;
        let hostname = Hostname::from_str(name)?;

        let target = match name {
            n if n.ends_with(ServiceTarget::SUBDOMAIN) => {
                let parts: Vec<_> = hostname.split('.').collect();
                if parts.len() != 5 {
                    return Err(Error::new_static(
                        "invalid Service target: name and namespace must be valid DNS labels",
                    ));
                }

                let name = parts[0].parse()?;
                let namespace = parts[1].parse()?;
                Target::Service(ServiceTarget {
                    name,
                    namespace,
                    port,
                })
            }
            _ => Target::DNS(DNSTarget { hostname, port }),
        };
        Ok(target)
    }

    #[doc(hidden)]
    pub fn from_passthrough_route_name(name: &str) -> Result<Self, Error> {
        let (name, port) = parse_port(name)?;
        let hostname = Hostname::from_str(name)?;

        let Some(hostname) = hostname.strip_suffix(Target::BACKEND_SUBDOMAIN) else {
            return Err(Error::new_static("expected a Junction backend name"));
        };

        let mut target = Self::from_name(hostname)?;
        if let Some(port) = port {
            target = target.with_port(port);
        }
        Ok(target)
    }

    pub fn port(&self) -> Option<u16> {
        match self {
            Target::DNS(dns) => dns.port,
            Target::Service(svc) => svc.port,
        }
    }

    /// Return a clone of this Target with its port set.
    pub fn with_port(&self, port: u16) -> Self {
        match self {
            Target::DNS(c) => Target::DNS(DNSTarget {
                port: Some(port),
                hostname: c.hostname.clone(),
            }),

            Target::Service(c) => Target::Service(ServiceTarget {
                port: Some(port),
                name: c.name.clone(),
                namespace: c.namespace.clone(),
            }),
        }
    }

    /// Clone this Target, replacing its current port with `None`.
    pub fn without_port(&self) -> Self {
        match self.clone() {
            Target::DNS(dns) => Target::DNS(DNSTarget { port: None, ..dns }),
            Target::Service(svc) => Target::Service(ServiceTarget { port: None, ..svc }),
        }
    }

    /// Return a clone of this Target with a port set to `default_port` if it
    /// doesn't already have a port.
    pub fn with_default_port(&self, default_port: u16) -> Self {
        match self {
            Target::DNS(dns) => match dns.port {
                Some(_) => self.clone(),
                None => self.clone().with_port(default_port),
            },
            Target::Service(svc) => match svc.port {
                Some(_) => self.clone(),
                None => self.clone().with_port(default_port),
            },
        }
    }
}

#[inline]
fn parse_port(s: &str) -> Result<(&str, Option<u16>), Error> {
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
    fn test_parses_json() {
        let target = serde_json::json!({
            "type": "service",
            "name": "foo",
            "namespace": "potato",
        });

        assert_eq!(
            serde_json::from_value::<Target>(target).unwrap(),
            Target::Service(ServiceTarget {
                name: Name::from_static("foo"),
                namespace: Name::from_static("potato"),
                port: None
            }),
        );

        let target = serde_json::json!({
            "type": "dns",
            "hostname": "example.com"
        });

        assert_eq!(
            serde_json::from_value::<Target>(target).unwrap(),
            Target::DNS(DNSTarget {
                hostname: Hostname::from_static("example.com"),
                port: None
            }),
        );
    }

    #[test]
    fn test_target_route_name() {
        assert_route_name(
            Target::Service(ServiceTarget {
                name: Name::from_static("potato"),
                namespace: Name::from_static("production"),
                port: None,
            }),
            "potato.production.svc.cluster.local",
        );

        assert_route_name(
            Target::Service(ServiceTarget {
                name: Name::from_static("potato"),
                namespace: Name::from_static("production"),
                port: Some(443),
            }),
            "potato.production.svc.cluster.local:443",
        );

        assert_route_name(
            Target::DNS(DNSTarget {
                hostname: Hostname::from_static("cool-stuff.example.com"),
                port: None,
            }),
            "cool-stuff.example.com",
        );

        assert_route_name(
            Target::DNS(DNSTarget {
                hostname: Hostname::from_static("cool-stuff.example.com"),
                port: Some(1234),
            }),
            "cool-stuff.example.com:1234",
        );
    }

    #[track_caller]
    fn assert_route_name(target: Target, str: &'static str) {
        assert_eq!(&target.name(), str);
        let parsed = Target::from_name(str).unwrap();
        assert_eq!(parsed, target);
    }

    #[test]
    fn test_target_backend_name() {
        assert_backend_name(
            Target::Service(ServiceTarget {
                name: Name::from_static("potato"),
                namespace: Name::from_static("production"),
                port: None,
            }),
            "potato.production.svc.cluster.local.lb.jct",
        );

        assert_backend_name(
            Target::Service(ServiceTarget {
                name: Name::from_static("potato"),
                namespace: Name::from_static("production"),
                port: Some(443),
            }),
            "potato.production.svc.cluster.local.lb.jct:443",
        );

        assert_backend_name(
            Target::DNS(DNSTarget {
                hostname: Hostname::from_static("cool-stuff.example.com"),
                port: None,
            }),
            "cool-stuff.example.com.lb.jct",
        );

        assert_backend_name(
            Target::DNS(DNSTarget {
                hostname: Hostname::from_static("cool-stuff.example.com"),
                port: Some(1234),
            }),
            "cool-stuff.example.com.lb.jct:1234",
        );
    }

    #[track_caller]
    fn assert_backend_name(target: Target, str: &'static str) {
        assert_eq!(&target.passthrough_route_name(), str);
        let parsed = Target::from_passthrough_route_name(str).unwrap();
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
        let len: usize = u.choose_index(max_len).unwrap();
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
