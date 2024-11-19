use std::collections::BTreeMap;
use std::str::FromStr;

use crate::backend::{Backend, LbPolicy};
use crate::error::{Error, ErrorContext};
use crate::{Name, Target};

use k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec};
use kube::api::ObjectMeta;
use kube::{Resource, ResourceExt};

const LB_ANNOTATION: &str = "junctionlabs.io/backend.lb";

fn lb_policy_annotation(port: u16) -> String {
    format!("{LB_ANNOTATION}.{port}")
}

impl Backend {
    /// Generate a partial [Service] from this backend.
    ///
    /// This service can be used to patch and overwrite an existing Service
    /// using the `kube` crate or saved as json/yaml and used to patch an
    /// existing service with `kubectl patch`.
    pub fn to_service_patch(&self) -> Service {
        let mut svc = Service {
            metadata: ObjectMeta {
                annotations: Some(BTreeMap::new()),
                ..Default::default()
            },
            ..Default::default()
        };

        let lb_annotation = lb_policy_annotation(self.id.port);
        let lb_json = serde_json::to_string(&self.lb)
            .expect("Failed to serialize Backend. this is a bug in Junction, not your code");
        svc.annotations_mut()
            .insert(lb_annotation.to_string(), lb_json);

        match &self.id.target {
            Target::Dns(dns) => {
                svc.spec = Some(ServiceSpec {
                    type_: Some("ExternalName".to_string()),
                    external_name: Some(dns.hostname.to_string()),
                    ..Default::default()
                })
            }
            Target::KubeService(service) => {
                let meta = svc.meta_mut();
                meta.name = Some(service.name.to_string());
                meta.namespace = Some(service.namespace.to_string());

                svc.spec = Some(ServiceSpec {
                    type_: Some("ClusterIP".to_string()),
                    ports: Some(vec![ServicePort {
                        port: self.id.port as i32,
                        protocol: Some("TCP".to_string()),
                        ..Default::default()
                    }]),
                    ..Default::default()
                })
            }
        };

        svc
    }

    /// Read one or more [Backend]s from a Kubernetes [Service]. A backend will
    /// be generated for every distinct port the [Service] is configured with.
    ///
    /// The type of [Backend] generated depends on the Service.
    ///
    /// - `ClusterIP` Services are translated to backends with a KubeService
    ///   target and that use the `port` of the Service and the address of each
    ///   endpoint. `ClusterIP` services must *not* be configured as headless
    ///   services, so that endpoint information is available.
    ///
    /// - `ExternalName` Services are translated to backends with a Dns target,
    ///    and uses the service port as the target port. If no port is specified,
    ///    backends are generated for ports 80 and 443.
    ///
    /// All other Service types are currently unsupported.
    pub fn from_service(svc: &Service) -> Result<Vec<Self>, Error> {
        let (namespace, name) = (
            as_ref_or_else(&svc.meta().namespace, "missing namespace")
                .with_fields("meta", "name")?,
            as_ref_or_else(&svc.meta().name, "missing name").with_fields("meta", "name")?,
        );

        let spec = as_ref_or_else(&svc.spec, "missing spec").with_field("spec")?;
        let svc_type = spec
            .type_
            .as_deref()
            .ok_or_else(|| Error::new_static("missing type"))
            .with_fields("spec", "type")?;

        let mut backends = vec![];

        // generate the target from the kube Service type.
        let (target, svc_ports) = match svc_type {
            "ClusterIP" => {
                let name = Name::from_str(name).with_fields("meta", "name")?;
                let namespace = Name::from_str(namespace).with_fields("meta", "namespace")?;
                let target = Target::kube_service(&namespace, &name)?;

                let svc_ports =
                    as_ref_or_else(&spec.ports, "missing ports").with_fields("spec", "ports")?;

                let mut ports = Vec::with_capacity(svc_ports.len());
                for (i, svc_port) in svc_ports.iter().enumerate() {
                    let port: u16 = convert_port(svc_port.port)
                        .with_field("port")
                        .with_field_index("ports", i)?;
                    ports.push(port);
                }

                (target, ports)
            }
            "ExternalName" => {
                let external_name = as_ref_or_else(&spec.external_name, "missing externalName")
                    .with_fields("spec", "externalName")?;

                let target = Target::dns(external_name).with_fields("spec", "externalName")?;
                let svc_ports = spec.ports.as_deref().unwrap_or_default();

                let mut ports = Vec::with_capacity(svc_ports.len());
                for (i, svc_port) in svc_ports.iter().enumerate() {
                    let port: u16 = convert_port(svc_port.port)
                        .with_field("port")
                        .with_field_index("ports", i)?;
                    ports.push(port);
                }

                if ports.is_empty() {
                    ports.extend([80, 443]);
                }

                (target, ports)
            }
            svc_type => return Err(Error::new(format!("{svc_type} Services are unsupported"))),
        };

        // generate a new Backend for every service port
        for port in svc_ports {
            let lb =
                get_lb_policy(svc.annotations(), &lb_policy_annotation(port))?.unwrap_or_default();
            backends.push(Backend {
                id: crate::BackendId {
                    target: target.clone(),
                    port,
                },
                lb,
            })
        }

        Ok(backends)
    }
}

fn get_lb_policy(
    annotations: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<LbPolicy>, Error> {
    match annotations.get(key) {
        Some(s) => {
            let lb_policy = serde_json::from_str(s)
                .map_err(|e| Error::new(format!("failed to deserialize {key}: {e}")))?;
            Ok(Some(lb_policy))
        }
        None => Ok(None),
    }
}

#[inline]
fn convert_port(port: i32) -> Result<u16, Error> {
    port.try_into()
        .map_err(|_| Error::new(format!("port value '{port}' is out of range")))
}

#[inline]
fn as_ref_or_else<'a, T>(f: &'a Option<T>, message: &'static str) -> Result<&'a T, Error> {
    f.as_ref().ok_or_else(|| Error::new_static(message))
}

#[cfg(test)]
mod test {
    use k8s_openapi::api::core::v1::{ServicePort, ServiceSpec};
    use kube::api::ObjectMeta;

    use crate::backend::{RingHashParams, SessionAffinityHashParam, SessionAffinityHashParamType};

    use super::*;

    macro_rules! annotations {
        ($($k:expr => $v:expr),* $(,)*) => {{
            let mut annotations = BTreeMap::new();
            $(
                annotations.insert($k.to_string(), $v.to_string());
            )*
            annotations
        }}
    }

    const CLUSTER_IP: Option<&str> = Some("ClusterIP");
    const EXTERNAL_NAME: Option<&str> = Some("ExternalName");

    #[test]
    fn test_to_service_patch() {
        let backend = Backend {
            id: Target::kube_service("bar", "foo")
                .unwrap()
                .into_backend(1212),
            lb: LbPolicy::RoundRobin,
        };
        assert_eq!(
            backend.to_service_patch(),
            Service {
                metadata: ObjectMeta {
                    namespace: Some("bar".to_string()),
                    name: Some("foo".to_string()),
                    annotations: Some(
                        annotations! { "junctionlabs.io/backend.lb.1212" => r#"{"type":"RoundRobin"}"# }
                    ),
                    ..Default::default()
                },
                spec: Some(ServiceSpec {
                    type_: CLUSTER_IP.map(str::to_string),
                    ports: Some(vec![ServicePort {
                        port: 1212,
                        protocol: Some("TCP".to_string()),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                status: None,
            }
        );

        let backend = Backend {
            id: Target::dns("example.com").unwrap().into_backend(4430),
            lb: LbPolicy::RoundRobin,
        };
        assert_eq!(
            backend.to_service_patch(),
            Service {
                metadata: ObjectMeta {
                    annotations: Some(
                        annotations! { "junctionlabs.io/backend.lb.4430" => r#"{"type":"RoundRobin"}"# }
                    ),
                    ..Default::default()
                },
                spec: Some(ServiceSpec {
                    type_: Some("ExternalName".to_string()),
                    external_name: Some("example.com".to_string()),
                    ..Default::default()
                }),
                status: None,
            }
        );
    }

    #[test]
    fn test_from_clusterip() {
        // should generate a backend for each port
        let svc = Service {
            metadata: ObjectMeta {
                namespace: Some("bar".to_string()),
                name: Some("foo".to_string()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: CLUSTER_IP.map(str::to_string),
                ports: Some(vec![ServicePort {
                    port: 8910,
                    protocol: Some("TCP".to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            status: None,
        };

        assert_eq!(
            Backend::from_service(&svc).unwrap(),
            vec![Backend {
                id: Target::kube_service("bar", "foo")
                    .unwrap()
                    .into_backend(8910),
                lb: LbPolicy::Unspecified,
            },]
        );

        // should error with no ports
        let no_ports = Service {
            metadata: ObjectMeta {
                namespace: Some("bar".to_string()),
                name: Some("foo".to_string()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: CLUSTER_IP.map(str::to_string),
                ..Default::default()
            }),
            status: None,
        };
        assert!(Backend::from_service(&no_ports).is_err());

        // multiple ports and some LB config, should generate different backends
        // with different LB policies.
        let svc = Service {
            metadata: ObjectMeta {
                namespace: Some("bar".to_string()),
                name: Some("foo".to_string()),
                annotations: Some(annotations! {
                    "junctionlabs.io/backend.lb.443" => r#"{"type":"RingHash", "min_ring_size": 1024, "hash_params": [{"type": "Header", "name": "x-user"}]}"#,
                    "junctionlabs.io/backend.lb.4430" => r#"{"type":"RoundRobin"}"#,
                }),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: CLUSTER_IP.map(str::to_string),
                ports: Some(vec![
                    ServicePort {
                        name: Some("http".to_string()),
                        port: 80,
                        protocol: Some("TCP".to_string()),
                        ..Default::default()
                    },
                    ServicePort {
                        name: Some("https".to_string()),
                        port: 443,
                        protocol: Some("TCP".to_string()),
                        ..Default::default()
                    },
                    ServicePort {
                        name: Some("health".to_string()),
                        port: 4430,
                        protocol: Some("TCP".to_string()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
            status: None,
        };

        assert_eq!(
            Backend::from_service(&svc).unwrap(),
            vec![
                Backend {
                    id: Target::kube_service("bar", "foo").unwrap().into_backend(80),
                    lb: LbPolicy::Unspecified,
                },
                Backend {
                    id: Target::kube_service("bar", "foo")
                        .unwrap()
                        .into_backend(443),
                    lb: LbPolicy::RingHash(RingHashParams {
                        min_ring_size: 1024,
                        hash_params: vec![SessionAffinityHashParam {
                            terminal: false,
                            matcher: SessionAffinityHashParamType::Header {
                                name: "x-user".to_string()
                            }
                        }]
                    }),
                },
                Backend {
                    id: Target::kube_service("bar", "foo")
                        .unwrap()
                        .into_backend(4430),
                    lb: LbPolicy::RoundRobin,
                },
            ]
        )
    }

    #[test]
    fn test_from_external_name() {
        // without explicit ports, should generate backends for both 443 and 80.
        // annotations should still get picked up.
        let svc = Service {
            metadata: ObjectMeta {
                namespace: Some("bar".to_string()),
                name: Some("foo".to_string()),
                annotations: Some(annotations! {
                    "junctionlabs.io/backend.lb.443" => r#"{"type":"RoundRobin"}"#,
                }),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: EXTERNAL_NAME.map(str::to_string),
                external_name: Some("www.junctionlabs.io".to_string()),
                ..Default::default()
            }),
            status: None,
        };

        assert_eq!(
            Backend::from_service(&svc).unwrap(),
            vec![
                Backend {
                    id: Target::dns("www.junctionlabs.io").unwrap().into_backend(80),
                    lb: LbPolicy::Unspecified,
                },
                Backend {
                    id: Target::dns("www.junctionlabs.io")
                        .unwrap()
                        .into_backend(443),
                    lb: LbPolicy::RoundRobin,
                },
            ]
        );

        // with explicit ports, we should use the given port and pick up an lb policy
        let svc = Service {
            metadata: ObjectMeta {
                namespace: Some("bar".to_string()),
                name: Some("foo".to_string()),
                annotations: Some(annotations! {
                    "junctionlabs.io/backend.lb.7777" => r#"{"type":"RoundRobin"}"#,
                }),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: EXTERNAL_NAME.map(str::to_string),
                external_name: Some("www.junctionlabs.io".to_string()),
                ports: Some(vec![ServicePort {
                    port: 7777,
                    protocol: Some("TCP".to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            status: None,
        };

        assert_eq!(
            Backend::from_service(&svc).unwrap(),
            vec![Backend {
                id: Target::dns("www.junctionlabs.io")
                    .unwrap()
                    .into_backend(7777),
                lb: LbPolicy::RoundRobin,
            },]
        )
    }

    #[test]
    fn test_svc_patch_roundtrip() {
        let backend = Backend {
            id: Target::kube_service("bar", "foo")
                .unwrap()
                .into_backend(8888),
            lb: LbPolicy::RoundRobin,
        };

        assert_eq!(
            Backend::from_service(&backend.to_service_patch()).unwrap(),
            vec![backend.clone()]
        )
    }
}
