use std::str::FromStr;

use crate::{
    error::{Error, ErrorContext},
    shared::{Regex, SessionAffinity, SessionAffinityHashParam, Target, WeightedTarget},
};

use xds_api::pb::envoy::config::route::v3 as xds_route;

pub(crate) fn parse_xds_regex(
    p: &xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher,
) -> Result<Regex, Error> {
    Regex::from_str(&p.regex).map_err(|e| Error::new(format!("invalid regex: {e}")))
}

pub(crate) fn regex_matcher(
    regex: &Regex,
) -> xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher {
    xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher {
        regex: regex.to_string(),
        engine_type: None,
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

impl WeightedTarget {
    pub(crate) fn to_xds(targets: &[Self]) -> Option<xds_route::route_action::ClusterSpecifier> {
        match targets {
            [] => None,
            [target] => Some(xds_route::route_action::ClusterSpecifier::Cluster(
                target.target.xds_cluster_name(),
            )),
            targets => {
                let clusters = targets
                    .iter()
                    .map(|wt| xds_route::weighted_cluster::ClusterWeight {
                        name: wt.target.xds_cluster_name(),
                        weight: Some(wt.weight.into()),
                        ..Default::default()
                    })
                    .collect();

                Some(xds_route::route_action::ClusterSpecifier::WeightedClusters(
                    xds_route::WeightedCluster {
                        clusters,
                        ..Default::default()
                    },
                ))
            }
        }
    }

    pub(crate) fn from_xds(
        xds: Option<&xds_route::route_action::ClusterSpecifier>,
    ) -> Result<Vec<Self>, Error> {
        match xds {
            Some(xds_route::route_action::ClusterSpecifier::Cluster(name)) => Ok(vec![Self {
                target: Target::from_cluster_xds_name(name).with_field("cluster")?,
                weight: 1,
            }]),
            Some(xds_route::route_action::ClusterSpecifier::WeightedClusters(
                weighted_clusters,
            )) => {
                let clusters = weighted_clusters.clusters.iter().enumerate().map(|(i, w)| {
                    let target =
                        Target::from_cluster_xds_name(&w.name).with_field_index("name", i)?;
                    let weight = crate::value_or_default!(w.weight, 1);

                    Ok(Self { target, weight })
                });

                clusters
                    .collect::<Result<Vec<_>, _>>()
                    .with_fields("weighted_clusters", "clusters")
            }
            Some(_) => Err(Error::new_static("unsupporetd cluster specifier")),
            None => Err(Error::new_static("missing cluster specifier")),
        }
    }
}
