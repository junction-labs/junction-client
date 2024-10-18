//! Junction API Configuration.
//!
//! These types let you express routing and load balancing configuration as data
//! structures. The `kube` and `xds` features of this crate let you convert
//! Junction configuration to Kubernetes objects and xDS resources respectively.
//!
//! Use this crate directly if you're looking to build and export configuration.
//! Use the `junction-client` crate if you want to make requests and resolve
//! addresses with Junction.

#[cfg(any(feature = "kube", feature = "xds"))]
mod error;

#[cfg(any(feature = "kube", feature = "xds"))]
pub use error::Error;

pub mod backend;
pub mod http;

mod shared;
pub use shared::{Duration, Fraction, Regex};

#[cfg(feature = "xds")]
mod xds;

#[cfg(feature = "kube")]
mod kube;

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "xds")]
macro_rules! value_or_default {
    ($value:expr, $default:expr) => {
        $value.as_ref().map(|v| v.value).unwrap_or($default)
    };
}

#[cfg(feature = "xds")]
pub(crate) use value_or_default;

/// The fully qualified domain name of a network host. This matches the RFC 1123 definition of a
/// hostname with 1 notable exception that numeric IP addresses are not allowed.
pub type PreciseHostname = String;

/// Defines a network port.
pub type PortNumber = u16;

#[derive(
    Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, Default, JsonSchema, PartialOrd, Ord,
)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct ServiceTarget {
    ///
    /// The name of the Kubernetes Service
    ///
    pub name: String,

    ///
    /// The namespace of the Kubernetes service. FIXME(namespace): what should the semantic be when
    /// this is not specified: default, namespace of client, namespace of EZbake?
    ///
    pub namespace: String,

    ///
    /// The port number of the Kubernetes service to target/ attach to.
    ///
    /// When attaching policies, if it is not specified, the target will apply to all connections
    /// that don't have a specific port specified.
    ///
    /// When being used to lookup a backend after a matched rule, if it is not specified then it
    /// will use the same port as the incoming request
    ///
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
}

#[derive(
    Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, Default, JsonSchema, PartialOrd, Ord,
)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct DNSTarget {
    ///
    /// The DNS Name to target/attach to
    ///
    pub hostname: PreciseHostname,

    ///
    /// The port number to target/attach to.
    ///
    /// When attaching policies, if it is not specified, the target will apply to all connections
    /// that don't have a specific port specified.
    ///
    /// When being used to lookup a backend after a matched rule, if it is not specified then it
    /// will use the same port as the incoming request
    ///
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
}

#[derive(
    Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, JsonSchema, PartialOrd, Ord,
)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum Target {
    DNS(DNSTarget),

    #[serde(untagged)]
    Service(ServiceTarget),
}

static KUBE_SERVICE_SUFFIX: &str = ".svc.cluster.local";

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_hostname())?;

        if let Some(port) = self.port() {
            f.write_fmt(format_args!(":{port}"))?;
        }

        Ok(())
    }
}

///
///  FIXME(ports): nothing with ports here will work until we move to xdstp
///
///  FIXME(DNS): For kube service hostnames, forcing the use of the ".svc.cluster.local" suffix is
///  icky,in that it stops the current k8s behavior of clients sending a request to
///  http://service_name, and having the namespace resolved based on where the client is running.
///
///  However without it, we are going to have a hard time here saying when an incoming hostname is
///  targeted at DNS, vs when it is targeted at a kube service. That might not be a problem though,
///  in that if we can process XDS NACKs, we can use something at a higher level to work out what it
///  is.
///
///  So punting thinking more about this until we support DNS properly.
///
impl Target {
    pub fn as_hostname(&self) -> String {
        match self {
            Target::DNS(c) => c.hostname.clone(),
            Target::Service(c) => format!("{}.{}{}", c.name, c.namespace, KUBE_SERVICE_SUFFIX),
        }
    }

    pub fn from_hostname(hostname: &str, port: Option<u16>) -> Self {
        if hostname.ends_with(KUBE_SERVICE_SUFFIX) {
            let mut parts = hostname.split('.');
            let name = parts.next().unwrap().to_string();
            let namespace = parts.next().unwrap().to_string();
            Target::Service(ServiceTarget {
                name,
                namespace,
                port,
            })
        } else {
            Target::DNS(DNSTarget {
                hostname: hostname.to_string(),
                port,
            })
        }
    }

    // FIXME(DNS): no reason we cannot support DNS here, but likely it should be determined by whats
    // in the cluster config so passed as a extra parameter
    #[cfg(feature = "xds")]
    pub fn from_cluster_xds_name(name: &str) -> Result<Self, Error> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 3 && parts[2].eq("cluster") {
            Ok(Target::Service(ServiceTarget {
                name: parts[1].to_string(),
                namespace: parts[0].to_string(),
                port: None,
            }))
        } else {
            Err(Error::new_static("unable to cluster name"))
        }
    }

    pub fn xds_cluster_name(&self) -> String {
        match self {
            // FIXME: this is wrong
            Target::DNS(c) => c.hostname.clone(),
            Target::Service(c) => format!("{}/{}/cluster", c.namespace, c.name),
        }
    }

    pub fn xds_endpoints_name(&self) -> String {
        match self {
            // FIXME: this is wrong
            Target::DNS(c) => c.hostname.clone(),
            Target::Service(c) => format!("{}/{}/endpoints", c.namespace, c.name),
        }
    }

    pub fn from_listener_xds_name(name: &str) -> Option<Self> {
        //for now, the listener name is just the hostname
        Some(Self::from_hostname(name, None))
    }

    pub fn xds_listener_name(&self) -> String {
        //FIXME(ports): for now this is just the hostname, with no support for port
        self.as_hostname()
    }

    pub fn xds_default_listener_name(&self) -> String {
        match self {
            Target::DNS(c) => format!("{hostname}/default", hostname = c.hostname),
            Target::Service(c) => {
                format!("{}.{}.junction.default", c.name, c.namespace)
            }
        }
    }

    pub fn port(&self) -> Option<u16> {
        match self {
            Target::DNS(c) => c.port,
            //FIXME(namespace): work out what to do if namespace is optional
            Target::Service(c) => c.port,
        }
    }

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
}

#[cfg(test)]
mod test_target {

    use crate::{ServiceTarget, Target};
    use serde_json::json;

    #[test]
    fn test_parses_target() {
        let target = json!({
            "type": "service",
            "name": "foo",
            "namespace": "potato",
        });

        assert_eq!(
            serde_json::from_value::<Target>(target).unwrap(),
            Target::Service(ServiceTarget {
                name: "foo".to_string(),
                namespace: "potato".to_string(),
                port: None
            }),
        )
    }
}
