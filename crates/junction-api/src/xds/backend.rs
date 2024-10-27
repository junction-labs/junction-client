use xds_api::pb::envoy::config::{
    cluster::v3::{self as xds_cluster, cluster::ring_hash_lb_config::HashFunction},
    route::v3 as xds_route,
};

use crate::{
    backend::{
        Backend, LbPolicy, RingHashParams, SessionAffinity, SessionAffinityHashParam,
        SessionAffinityHashParamType,
    },
    error::{Error, ErrorContext},
    value_or_default, BackendTarget, Target,
};

impl Backend {
    pub fn from_xds(
        cluster: &xds_cluster::Cluster,
        route_action: Option<&xds_route::RouteAction>,
    ) -> Result<Self, Error> {
        let lb = LbPolicy::from_xds(cluster, route_action)?;
        let target = BackendTarget::from_name(&cluster.name)?;
        Ok(Backend { target, lb })
    }

    pub fn to_xds_cluster(&self) -> xds_cluster::Cluster {
        use xds_cluster::cluster::ClusterDiscoveryType;
        use xds_cluster::cluster::DiscoveryType;
        use xds_cluster::cluster::EdsClusterConfig;

        let (lb_policy, lb_config) = match self.lb.to_xds() {
            Some((policy, config)) => (policy, Some(config)),
            None => (xds_cluster::cluster::LbPolicy::default(), None),
        };

        let cluster_discovery_type = match &self.target.target {
            Target::Dns(_) => ClusterDiscoveryType::Type(DiscoveryType::LogicalDns.into()),
            Target::Service(_) => ClusterDiscoveryType::Type(DiscoveryType::Eds.into()),
        };

        xds_cluster::Cluster {
            name: self.target.name(),
            lb_policy: lb_policy.into(),
            lb_config,
            cluster_discovery_type: Some(cluster_discovery_type),
            eds_cluster_config: Some(EdsClusterConfig {
                eds_config: Some(crate::xds::ads_config_source()),
                service_name: self.target.name(),
            }),
            ..Default::default()
        }
    }

    pub fn to_xds_passthrough_route(&self) -> xds_route::RouteConfiguration {
        use xds_route::route::Action;
        use xds_route::route_action::ClusterSpecifier;
        use xds_route::route_match::PathSpecifier;

        let default_action = Action::Route(xds_route::RouteAction {
            cluster_specifier: Some(ClusterSpecifier::Cluster(self.target.name())),
            hash_policy: self.to_xds_hash_policies(),
            ..Default::default()
        });

        let default_route = xds_route::Route {
            r#match: Some(xds_route::RouteMatch {
                path_specifier: Some(PathSpecifier::Prefix("".to_string())),
                ..Default::default()
            }),
            action: Some(default_action),
            ..Default::default()
        };

        let vhost = xds_route::VirtualHost {
            domains: vec!["*".to_string()],
            routes: vec![default_route],
            ..Default::default()
        };

        // NOTE: this is the only place where we use `backend_name` to name a
        // RouteConfiguration.
        //
        // it's incorrect literally everywhere else, but should be done here to
        // indicate that this is the passthrough route.
        xds_route::RouteConfiguration {
            name: self.target.passthrough_route_name(),
            virtual_hosts: vec![vhost],
            ..Default::default()
        }
    }

    fn to_xds_hash_policies(&self) -> Vec<xds_route::route_action::HashPolicy> {
        match &self.lb {
            LbPolicy::RingHash(ring_hash) => {
                ring_hash.hash_params.iter().map(|p| p.to_xds()).collect()
            }
            _ => Vec::new(),
        }
    }
}

impl LbPolicy {
    pub(crate) fn from_xds(
        cluster: &xds_cluster::Cluster,
        route_action: Option<&xds_route::RouteAction>,
    ) -> Result<Self, Error> {
        match cluster.lb_policy() {
            // for ROUND_ROBIN, ignore the slow_start_config entirely and return a brand new
            // RoundRobin policy each time. validate that the config matches the enum field even
            // though it's ignored.
            xds_cluster::cluster::LbPolicy::RoundRobin => match cluster.lb_config.as_ref() {
                Some(xds_cluster::cluster::LbConfig::RoundRobinLbConfig(_)) => {
                    Ok(LbPolicy::RoundRobin)
                }
                None => Ok(LbPolicy::Unspecified),
                _ => Err(
                    Error::new_static("RoundRobin lb_policy has a mismatched lb_config")
                        .with_field("lb_config"),
                ),
            },
            // for RING_HASH pull the config out if set or use default values to populate our
            // config.
            xds_cluster::cluster::LbPolicy::RingHash => {
                let lb_config = match cluster.lb_config.as_ref() {
                    Some(xds_cluster::cluster::LbConfig::RingHashLbConfig(config)) => config,
                    None => &xds_cluster::cluster::RingHashLbConfig::default(),
                    _ => {
                        return Err(Error::new_static(
                            "RingHash lb_policy has a mismatched lb_config",
                        )
                        .with_field("lb_config"))
                    }
                };

                // hash function must be XX_HASH to match gRPC
                if lb_config.hash_function() != HashFunction::XxHash {
                    return Err(Error::new(format!(
                        "unsupported hash function: {:?}",
                        lb_config.hash_function(),
                    )))
                    .with_fields("lb_config", "hash_function");
                }

                let min_ring_size = value_or_default!(
                    lb_config.minimum_ring_size,
                    crate::backend::default_min_ring_size() as u64
                );
                let min_ring_size = min_ring_size
                    .try_into()
                    .map_err(|_| Error::new_static("int overflow"))
                    .with_fields("lb_config", ",minimum_ring_size")?;

                let hash_params = route_action
                    .map(hash_policies)
                    .transpose()
                    .with_field("route_action")?;

                Ok(LbPolicy::RingHash(RingHashParams {
                    min_ring_size,
                    hash_params: hash_params.unwrap_or_default(),
                }))
            }
            _ => Err(Error::new_static("unsupported lb policy")).with_field("lb_policy"),
        }
    }

    pub(crate) fn to_xds(
        &self,
    ) -> Option<(
        xds_cluster::cluster::LbPolicy,
        xds_cluster::cluster::LbConfig,
    )> {
        match self {
            LbPolicy::RoundRobin => Some((
                xds_cluster::cluster::LbPolicy::RoundRobin,
                xds_cluster::cluster::LbConfig::RoundRobinLbConfig(Default::default()),
            )),
            LbPolicy::RingHash(params) => Some((
                xds_cluster::cluster::LbPolicy::RingHash,
                xds_cluster::cluster::LbConfig::RingHashLbConfig(
                    xds_cluster::cluster::RingHashLbConfig {
                        minimum_ring_size: Some((params.min_ring_size as u64).into()),
                        hash_function:
                            xds_cluster::cluster::ring_hash_lb_config::HashFunction::XxHash as i32,
                        maximum_ring_size: None,
                    },
                ),
            )),
            // an unspecified LB policy just sets LbConfig to none
            LbPolicy::Unspecified => None,
        }
    }
}

#[inline]
fn hash_policies(action: &xds_route::RouteAction) -> Result<Vec<SessionAffinityHashParam>, Error> {
    let res: Result<Vec<_>, Error> = action
        .hash_policy
        .iter()
        .enumerate()
        .map(|(i, policy)| SessionAffinityHashParam::from_xds(policy).with_index(i))
        .collect();

    res.with_field("hash_policy")
}

impl SessionAffinityHashParam {
    pub(crate) fn to_xds(&self) -> xds_route::route_action::HashPolicy {
        use xds_route::route_action::hash_policy::Header;
        use xds_route::route_action::hash_policy::PolicySpecifier;

        match &self.matcher {
            SessionAffinityHashParamType::Header { name } => xds_route::route_action::HashPolicy {
                terminal: self.terminal,
                policy_specifier: Some(PolicySpecifier::Header(Header {
                    header_name: name.clone(),
                    regex_rewrite: None,
                })),
            },
        }
    }

    pub(crate) fn from_xds(xds: &xds_route::route_action::HashPolicy) -> Result<Self, Error> {
        use xds_route::route_action::hash_policy::PolicySpecifier;

        match &xds.policy_specifier {
            Some(PolicySpecifier::Header(header)) => Ok(Self {
                terminal: xds.terminal,
                matcher: SessionAffinityHashParamType::Header {
                    name: header.header_name.clone(),
                },
            }),
            Some(_) => {
                Err(Error::new_static("unsupported hash policy").with_field("policy_specifier"))
            }
            None => Err(Error::new_static("no policy specified").with_field("policy_specifier")),
        }
    }
}

impl SessionAffinity {
    pub fn from_xds(
        hash_policy: &[xds_route::route_action::HashPolicy],
    ) -> Result<Option<Self>, Error> {
        if hash_policy.is_empty() {
            return Ok(None);
        }

        let hash_params = hash_policy
            .iter()
            .enumerate()
            .map(|(i, h)| SessionAffinityHashParam::from_xds(h).with_index(i))
            .collect::<Result<Vec<_>, _>>()?;

        debug_assert!(!hash_params.is_empty(), "hash params must not be empty");
        Ok(Some(Self { hash_params }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::http::Route;

    #[test]
    fn test_unspecified_lb_roundtrips() {
        let web = Target::kube_service("prod", "web").unwrap();

        let backend = Backend {
            target: web.into_backend(8891),
            lb: LbPolicy::Unspecified,
        };
        assert_eq!(
            backend,
            Backend::from_xds(&backend.to_xds_cluster(), None).unwrap(),
        );
        assert_eq!(backend.to_xds_hash_policies(), vec![]);
    }

    #[test]
    fn test_round_robin_lb_roundtrips() {
        let web = Target::kube_service("prod", "web").unwrap();

        let backend = Backend {
            target: web.into_backend(7891),
            lb: LbPolicy::RoundRobin,
        };
        assert_eq!(
            backend,
            Backend::from_xds(&backend.to_xds_cluster(), None).unwrap(),
        );
        assert_eq!(backend.to_xds_hash_policies(), vec![]);
    }

    #[test]
    fn test_ringhash_roundtrip() {
        let web = Target::kube_service("prod", "web").unwrap();

        let backend = Backend {
            target: web.into_backend(6666),
            lb: LbPolicy::RingHash(RingHashParams {
                min_ring_size: 1024,
                hash_params: vec![
                    SessionAffinityHashParam {
                        terminal: false,
                        matcher: SessionAffinityHashParamType::Header {
                            name: "x-user".to_string(),
                        },
                    },
                    SessionAffinityHashParam {
                        terminal: false,
                        matcher: SessionAffinityHashParamType::Header {
                            name: "x-env".to_string(),
                        },
                    },
                ],
            }),
        };

        let cluster = backend.to_xds_cluster();
        let hash_policy = backend.to_xds_hash_policies();

        let parsed = Backend::from_xds(
            &cluster,
            Some(&xds_route::RouteAction {
                hash_policy,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(parsed, backend);
    }

    #[test]
    fn test_passthrough_route_is_passthrough() {
        let web = Target::kube_service("prod", "web").unwrap();

        let backend = Backend {
            target: web.into_backend(4321),
            lb: LbPolicy::RoundRobin,
        };

        let route = Route::from_xds(&backend.to_xds_passthrough_route()).unwrap();
        assert!(route.is_passthrough_route());
    }

    #[test]
    fn test_passthrough_route_roundtrip() {
        let web = Target::kube_service("prod", "web").unwrap();

        let backend = Backend {
            target: web.into_backend(12321),
            lb: LbPolicy::RingHash(RingHashParams {
                min_ring_size: 1024,
                hash_params: vec![
                    SessionAffinityHashParam {
                        terminal: false,
                        matcher: SessionAffinityHashParamType::Header {
                            name: "x-user".to_string(),
                        },
                    },
                    SessionAffinityHashParam {
                        terminal: false,
                        matcher: SessionAffinityHashParamType::Header {
                            name: "x-env".to_string(),
                        },
                    },
                ],
            }),
        };

        let cluster = backend.to_xds_cluster();
        let passthrough_route = backend.to_xds_passthrough_route();

        let parsed = Backend::from_xds(&cluster, {
            let vhost = passthrough_route.virtual_hosts.first().unwrap();
            let route = vhost.routes.first().unwrap();
            route.action.as_ref().map(|action| match action {
                xds_route::route::Action::Route(action) => action,
                _ => panic!("invalid route"),
            })
        })
        .unwrap();
        assert_eq!(backend, parsed);
    }
}
