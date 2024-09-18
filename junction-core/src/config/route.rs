use serde::Deserialize;
use std::time::Duration;
use xds_api::pb::envoy::config::route::v3 as xds_route;

use crate::RetryPolicy;

// FIXME: to allow manual config, allow adding a route to the front of a virtualhost for any vhost that matches a domain

// FIXME: handle wildcard domains and domain search order instead of picking the first match
// FIXME: do something with retry policies
// FIXME: shadowing
// FIXME: aggregate clusters
// FIXME: dns backed clusters

/// A VirtualHost is a combination of Envoy's `Listener` and `VirtualHost`
/// concepts, applied to all URLs for a set of domains.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Route {
    /// The domains that this VirtualHost applies to. Domains are matched
    /// against the incoming authority of any URL.
    pub domains: Vec<String>,

    /// FIXME: add http filters
    // filters: Vec<()>,

    /// The route rules that determine whether any URLs match.
    ///
    /// A VirtualHost may not have an empty set of routes.
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

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RouteRule {
    pub timeout: Option<Duration>,

    pub retry_policy: Option<crate::RetryPolicy>,

    #[serde(default)]
    pub matches: Vec<RouteMatcher>,

    #[serde(default)]
    pub hash_policies: Vec<HashPolicy>,

    pub target: RouteTarget,
}

impl RouteRule {
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
            .any(|m| m.is_match(method, url, headers))
    }

    fn from_xds(route: &xds_route::Route) -> Option<Self> {
        let matcher = route.r#match.as_ref().and_then(RouteMatcher::from_xds)?;

        let Some(xds_route::route::Action::Route(action)) = route.action.as_ref() else {
            return None;
        };

        let timeout = action
            .timeout
            .as_ref()
            .map(|d| Duration::new(d.seconds as u64, d.nanos as u32));
        let retry_policy = action.retry_policy.as_ref().map(RetryPolicy::from_xds);

        let hash_policies = action
            .hash_policy
            .iter()
            .filter_map(HashPolicy::from_xds)
            .collect();

        let target = RouteTarget::from_xds(action.cluster_specifier.as_ref()?)?;

        Some(RouteRule {
            timeout,
            retry_policy,
            matches: vec![matcher],
            hash_policies,
            target,
        })
    }
}

// TODO: figure out how we want to support the filter_state/connection_properties
// style of hashing based on source ip or grpc channel.
//
// TODO: add support for query parameter based hashing, which involves parsing
// query parameters, which http::uri just doesn't do. switch the whole crate to
// url::Url or something.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HashPolicy {
    #[serde(default)]
    pub terminal: bool,

    #[serde(flatten)]
    pub target: HashTarget,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HashTarget {
    Header(String),
    Query(String),
}

impl HashPolicy {
    fn from_xds(hash_policy: &xds_route::route_action::HashPolicy) -> Option<Self> {
        use xds_route::route_action::hash_policy::PolicySpecifier;

        match hash_policy.policy_specifier.as_ref() {
            Some(PolicySpecifier::Header(h)) => Some(HashPolicy {
                terminal: hash_policy.terminal,
                target: HashTarget::Header(h.header_name.clone()),
            }),
            _ => None,
        }
    }
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

// FIXME: method, query
// FIXME: do we want to support runtime_fraction?
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RouteMatcher {
    pub path: StringMatcher,

    pub headers: Vec<HeaderMatcher>,
}

impl Default for RouteMatcher {
    fn default() -> Self {
        Self {
            path: StringMatcher::Any,
            headers: Vec::new(),
        }
    }
}

impl RouteMatcher {
    pub fn is_match(
        &self,
        _method: &http::Method,
        url: &crate::Url,
        headers: &http::HeaderMap,
    ) -> bool {
        self.path.is_match(url.path()) && any_header_match(&self.headers, headers)
    }

    pub fn from_xds(route_spec: &xds_route::RouteMatch) -> Option<Self> {
        let path = route_spec
            .path_specifier
            .as_ref()
            .and_then(StringMatcher::from_xds_path_spec)?;

        let headers = route_spec
            .headers
            .iter()
            .filter_map(HeaderMatcher::from_xds)
            .collect();

        Some(Self { path, headers })
    }
}

fn any_header_match(matchers: &[HeaderMatcher], headers: &http::HeaderMap) -> bool {
    if matchers.is_empty() {
        return true;
    }

    matchers.iter().any(|m| m.is_match(headers))
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(from = "String")]
pub enum StringMatcher {
    Any,
    Prefix(String),
    Exact(String),
}

impl From<String> for StringMatcher {
    fn from(value: String) -> Self {
        match value.as_ref() {
            "*" => Self::Any,
            _ => Self::Exact(value),
        }
    }
}

impl StringMatcher {
    pub fn is_match(&self, value: &str) -> bool {
        match self {
            StringMatcher::Any => true,
            StringMatcher::Prefix(prefix) => {
                value.len() > prefix.len() && value.starts_with(prefix)
            }
            StringMatcher::Exact(exact_match) => exact_match == value,
        }
    }

    fn any() -> Self {
        Self::Any
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HeaderMatcher {
    pub name: String,

    #[serde(default = "StringMatcher::any")]
    pub value: StringMatcher,
}

impl HeaderMatcher {
    fn is_match(&self, headers: &http::HeaderMap) -> bool {
        let Some(header_val) = headers.get(&self.name) else {
            return false;
        };

        let Ok(header_val) = header_val.to_str() else {
            return false;
        };

        self.value.is_match(header_val)
    }

    fn from_xds(header_matcher: &xds_route::HeaderMatcher) -> Option<Self> {
        let name = header_matcher.name.clone();
        let value = match header_matcher.header_match_specifier.as_ref()? {
            xds_route::header_matcher::HeaderMatchSpecifier::ExactMatch(s) => {
                StringMatcher::Exact(s.clone())
            }
            xds_route::header_matcher::HeaderMatchSpecifier::PresentMatch(true) => {
                StringMatcher::Any
            }
            xds_route::header_matcher::HeaderMatchSpecifier::PrefixMatch(pfx) => {
                StringMatcher::Prefix(pfx.clone())
            }
            _ => return None,
        };

        Some(Self { name, value })
    }
}

impl StringMatcher {
    fn from_xds_path_spec(path_spec: &xds_route::route_match::PathSpecifier) -> Option<Self> {
        match path_spec {
            xds_route::route_match::PathSpecifier::Prefix(p) => {
                if p == "/" {
                    Some(StringMatcher::Any)
                } else {
                    Some(StringMatcher::Prefix(p.clone()))
                }
            }
            xds_route::route_match::PathSpecifier::Path(p) => Some(StringMatcher::Exact(p.clone())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod matcher_test {
    use ::rand::{
        distributions::{Alphanumeric, DistString},
        seq::IteratorRandom,
    };

    use super::*;
    use crate::rand;

    #[test]
    fn test_match_any() {
        let m: StringMatcher = "*".to_string().into();
        assert_eq!(m, StringMatcher::Any);

        rand::with_thread_rng(|rng| {
            for _ in 0..30 {
                let str_len = (8..64).choose(rng).unwrap_or(16);
                let random_str = Alphanumeric.sample_string(rng, str_len);
                assert!(
                    m.is_match(&random_str),
                    "should have matched any string: failed on {random_str:?}"
                );
            }
        })
    }

    #[test]
    fn test_match_prefix() {
        let m = StringMatcher::Prefix("potato".to_string());

        assert!(
            !m.is_match("potato"),
            "prefix should not have matched by itself",
        );

        // prefix should match
        let a_string = "potatopancakes";
        assert!(
            m.is_match(a_string),
            "should have matched a prefix: {a_string:?}"
        );

        // no prefix shouldn't match
        let a_string = "lemonpancakes";
        assert!(
            !m.is_match(a_string),
            "should not have matched without a prefix: {a_string:?}"
        );
    }
}

#[cfg(test)]
mod json_test {
    use super::*;
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
                    timeout: None,
                    retry_policy: None,
                    matches: vec![],
                    hash_policies: vec![],
                    target: RouteTarget::Cluster("foo.bar".to_string()),
                }],
            },
        );
    }

    #[test]
    fn route_with_path_match() {
        assert_deserialize(
            json!({
                "domains": ["foo.bar"],
                "rules": [
                    {
                        "matches": [
                            {"path": "*"}
                        ],
                        "target": "foo.bar",
                    }
                ]
            }),
            Route {
                domains: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    timeout: None,
                    retry_policy: None,
                    matches: vec![RouteMatcher {
                        path: StringMatcher::Any,
                        headers: vec![],
                    }],
                    hash_policies: vec![],
                    target: RouteTarget::Cluster("foo.bar".to_string()),
                }],
            },
        );
    }

    #[test]
    fn route_with_header_name_match() {
        assert_deserialize(
            json!({
                "domains": ["foo.bar"],
                "rules": [
                    {
                        "matches": [
                            {
                                "headers": [{
                                    "name": "x-foo-whatever",
                                }],
                            },
                        ],
                        "target": "foo.bar",
                    }
                ]
            }),
            Route {
                domains: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    timeout: None,
                    retry_policy: None,
                    matches: vec![RouteMatcher {
                        path: StringMatcher::Any,
                        headers: vec![HeaderMatcher {
                            name: "x-foo-whatever".to_string(),
                            value: StringMatcher::Any,
                        }],
                    }],
                    hash_policies: vec![],
                    target: RouteTarget::Cluster("foo.bar".to_string()),
                }],
            },
        );
    }

    #[test]
    fn route_with_weighted_clusters() {
        assert_deserialize(
            json!({
                "domains": ["foo.bar"],
                "rules": [
                    {
                        "target": [
                            {"name": "foo.bar", "weight": 3},
                            {"name": "foo.baz", "weight": 1},
                        ],
                    }
                ]
            }),
            Route {
                domains: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    timeout: None,
                    retry_policy: None,
                    matches: vec![],
                    hash_policies: vec![],
                    target: RouteTarget::WeightedClusters(vec![
                        WeightedCluster {
                            name: "foo.bar".to_string(),
                            weight: 3,
                        },
                        WeightedCluster {
                            name: "foo.baz".to_string(),
                            weight: 1,
                        },
                    ]),
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
                        "hash_policies": [
                            {"header": "x-foo"},
                        ]
                    },
                ],
            }),
            Route {
                domains: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    timeout: None,
                    retry_policy: None,
                    matches: vec![],
                    hash_policies: vec![HashPolicy {
                        terminal: false,
                        target: HashTarget::Header("x-foo".to_string()),
                    }],
                    target: RouteTarget::Cluster("foo.bar".to_string()),
                }],
            },
        );

        assert_deserialize(
            json!({
                "domains": ["foo.bar"],
                "rules": [
                    {
                        "target": "foo.bar",
                        "hash_policies": [
                            {"query": "param"},
                        ]
                    },
                ],
            }),
            Route {
                domains: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    timeout: None,
                    retry_policy: None,
                    matches: vec![],
                    hash_policies: vec![HashPolicy {
                        terminal: false,
                        target: HashTarget::Query("param".to_string()),
                    }],
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
