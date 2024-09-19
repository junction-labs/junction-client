use junction_gateway_api::{
    gateway_api::httproute::{
        HTTPHeaderMatch, HTTPPathMatchType, HTTPQueryParamMatch, HTTPRouteFilter, HTTPRouteMatch,
        HTTPRouteTimeouts, StringMatchType,
    },
    jct_http_retry_policy::JctHTTPRetryPolicy,
    jct_http_session_affinity_policy::JctHTTPSessionAffinityPolicy,
};
use regex::Regex;
use serde::Deserialize;
use xds_api::pb::envoy::config::route::v3 as xds_route;

// FIXME: to allow manual config, allow adding a route to the front of a virtualhost for any vhost that matches a domain

// FIXME: handle wildcard domains and domain search order instead of picking the first match
// FIXME: do something with retry policies
// FIXME: shadowing
// FIXME: aggregate clusters
// FIXME: dns backed clusters

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    /// The domains that this VirtualHost applies to. Domains are matched
    /// against the incoming authority of any URL.
    pub domains: Vec<String>,

    /// The route rules that determine whether any URLs match.
    pub rules: Vec<RouteRule>,
}

impl Route {
    pub fn matching_rule(
        &self,
        method: &http::Method,
        url: &crate::Url,
        headers: &http::HeaderMap,
    ) -> Option<&RouteRule> {
        if !self.domains.iter().any(|d| d == "*" || d == url.hostname()) {
            return None;
        }

        self.rules
            .iter()
            .find(|rule| rule.is_match(method, url, headers))
    }
}

impl Route {
    pub fn from_xds(vhost: &xds_route::VirtualHost) -> Self {
        let domains = vhost.domains.clone();
        let rules: Vec<_> = vhost
            .routes
            .iter()
            .filter_map(RouteRule::from_xds)
            .collect();

        Route { domains, rules }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteRule {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<HTTPRouteMatch>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<HTTPRouteFilter>,
    // someday will add this support
    //pub session_persistence: Option<HTTPRouteRulesSessionPersistence>,
    //
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<HTTPRouteTimeouts>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity: Option<JctHTTPSessionAffinityPolicy>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<JctHTTPRetryPolicy>,

    pub target: RouteTarget,
}

impl RouteRule {
    // FIXME: this needs some type of max score, that can be compared to other
    // scores, to determine if it is the match
    pub fn is_match(
        &self,
        method: &http::Method,
        url: &crate::Url,
        headers: &http::HeaderMap,
    ) -> bool {
        if self.matches.is_empty() {
            return true;
        }

        self.matches
            .iter()
            .any(|m| is_rule_match(m, method, url, headers))
    }

    fn from_xds(route: &xds_route::Route) -> Option<Self> {
        let matches: HTTPRouteMatch = route.r#match.as_ref().and_then(HTTPRouteMatch::from_xds)?;

        let Some(xds_route::route::Action::Route(action)) = route.action.as_ref() else {
            return None;
        }; //FIXME: is this really right for filters?

        let timeouts: Option<HTTPRouteTimeouts> = HTTPRouteTimeouts::from_xds(action);

        let retry_policy = action
            .retry_policy
            .as_ref()
            .map(JctHTTPRetryPolicy::from_xds);

        let session_affinity = JctHTTPSessionAffinityPolicy::from_xds(&action.hash_policy);

        let target = RouteTarget::from_xds(action.cluster_specifier.as_ref()?)?;

        Some(RouteRule {
            matches: vec![matches],
            retry_policy,
            filters: vec![],
            session_affinity,
            timeouts,
            target,
        })
    }
}

pub fn is_rule_match(
    rule: &HTTPRouteMatch,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> bool {
    let mut method_matches = true;
    if let Some(rule_method) = &rule.method {
        method_matches = rule_method.eq(&method.to_string());
    }

    let mut path_matches = true;
    if let Some(rule_path) = &rule.path {
        path_matches = match &rule_path.r#type {
            HTTPPathMatchType::Exact => rule_path.value == url.path(),
            HTTPPathMatchType::PathPrefix => url.path().starts_with(&rule_path.value),
            HTTPPathMatchType::RegularExpression => eval_regex(&rule_path.value, url.path()),
        }
    }

    let headers_matches = rule.headers.iter().all(|m| is_header_match(m, headers));

    let qp_matches = rule
        .query_params
        .iter()
        .all(|m| is_query_params_match(m, url.query()));

    return method_matches && path_matches && headers_matches && qp_matches;
}

pub fn eval_regex(regex: &str, val: &str) -> bool {
    return match Regex::new(regex) {
        Ok(re) => re.is_match(val),
        Err(_) => false,
    };
}

pub fn is_header_match(rule: &HTTPHeaderMatch, headers: &http::HeaderMap) -> bool {
    let Some(header_val) = headers.get(&rule.name) else {
        return false;
    };
    let Ok(header_val) = header_val.to_str() else {
        return false;
    };
    return match &rule.r#type {
        StringMatchType::Exact => header_val == rule.value,
        StringMatchType::RegularExpression => eval_regex(&rule.value, header_val),
    };
}

pub fn is_query_params_match(rule: &HTTPQueryParamMatch, query: Option<&str>) -> bool {
    for (param, value) in form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        if param == rule.name {
            return match rule.r#type {
                StringMatchType::Exact => rule.value == value,
                StringMatchType::RegularExpression => eval_regex(&rule.value, &value),
            };
        }
    }
    return false;
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RouteTarget {
    Cluster(String),
    WeightedClusters(Vec<WeightedCluster>),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WeightedCluster {
    pub name: String,
    pub weight: u32,
}

impl RouteTarget {
    fn from_xds(cluster_spec: &xds_route::route_action::ClusterSpecifier) -> Option<Self> {
        match &cluster_spec {
            xds_route::route_action::ClusterSpecifier::Cluster(name) => {
                Some(Self::Cluster(name.clone()))
            }
            xds_route::route_action::ClusterSpecifier::WeightedClusters(weighted_cluster) => {
                let named_and_weights = weighted_cluster
                    .clusters
                    .iter()
                    .map(|w| WeightedCluster {
                        name: w.name.clone(),
                        weight: crate::xds::value_or_default!(w.weight, 0),
                    })
                    .collect();

                Some(Self::WeightedClusters(named_and_weights))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod json_test {
    use super::*;
    use junction_gateway_api::jct_http_session_affinity_policy::JctHTTPSessionAffinityHashParam;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    #[test]
    fn minimal_route() {
        assert_deserialize(
            json!({
                "domains": ["foo.bar"],
                "rules": [
                    {
                        "target": "foo.bar",
                    }
                ]
            }),
            Route {
                domains: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    matches: vec![],
                    filters: vec![],
                    timeouts: None,
                    session_affinity: None,
                    retry_policy: None,
                    target: RouteTarget::Cluster("foo.bar".to_string()),
                }],
            },
        );
    }

    #[test]
    fn minimal_route_missing_target() {
        assert_deserialize_err::<Route>(json!({
            "domains": ["foo.bar"],
            "rules": [
                {
                    "matches": [],
                }
            ]
        }));
    }

    #[test]
    fn route_with_hash_policy() {
        assert_deserialize(
            json!({
                "domains": ["foo.bar"],
                "rules": [
                    {
                        "target": "foo.bar",
                        "sessionAffinity": {
                            "hashParams": [ {"type": "Header", "name": "x-foo"} ],
                        }
                    },
                ],
            }),
            Route {
                domains: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    matches: vec![],
                    filters: vec![],
                    timeouts: None,
                    session_affinity: Some(JctHTTPSessionAffinityPolicy {
                        hash_params: vec![JctHTTPSessionAffinityHashParam { 
                            r#type: junction_gateway_api::jct_http_session_affinity_policy::JctHTTPSessionAffinityHashParamType::Header, 
                            name: "x-foo".to_string(), 
                            terminal: false 
                        } ],
                    }),
                    retry_policy: None,
                    target: RouteTarget::Cluster("foo.bar".to_string()),
                }],
            },
        );
    }

    fn assert_deserialize<T: DeserializeOwned + PartialEq + std::fmt::Debug>(
        json: serde_json::Value,
        expected: T,
    ) {
        let actual: T = serde_json::from_value(json).unwrap();
        assert_eq!(expected, actual);
    }

    fn assert_deserialize_err<T: DeserializeOwned + PartialEq + std::fmt::Debug>(
        json: serde_json::Value,
    ) -> serde_json::Error {
        serde_json::from_value::<T>(json).unwrap_err()
    }
}
