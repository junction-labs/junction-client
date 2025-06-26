use super::{ErrorCtx, Resource, ResourceError, ResourceName, ResourceType};

use junction_api::Hostname;
use regex::Regex;
use std::{collections::HashSet, str::FromStr, time::Duration};
use xds_api::pb::envoy::config::route::v3::{self as xds_route};

macro_rules! vec_from_xds {
    ($xs:expr, $field:literal, $type:ty) => {{
        $xs.iter()
            .enumerate()
            .map(|(i, x)| <$type>::from_xds(x).with_field_index($field, i))
            .collect::<Result<Vec<_>, _>>()
    }};
}

#[derive(Debug, Clone)]
pub(crate) struct RouteConfiguration {
    pub vhosts: Vec<VirtualHost>,
}

impl Resource for RouteConfiguration {
    type Xds = xds_route::RouteConfiguration;

    fn from_xds(xds: &Self::Xds) -> Result<Self, ResourceError> {
        let vhosts = vec_from_xds!(xds.virtual_hosts, "virtual_hosts", VirtualHost)?;
        Ok(Self { vhosts })
    }

    fn references(&self) -> Vec<(super::ResourceType, ResourceName)> {
        let mut clusters = HashSet::new();
        for vhost in &self.vhosts {
            for route in &vhost.routes {
                match &route.action.cluster {
                    ClusterSpecifier::Cluster(name) => {
                        clusters.insert(name.clone());
                    }
                    ClusterSpecifier::Weighted(weights) => {
                        for weight in weights {
                            clusters.insert(weight.name.clone());
                        }
                    }
                }
            }
        }

        clusters
            .into_iter()
            .map(|name| (ResourceType::Cluster, name))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VirtualHost {
    pub domains: Vec<DomainMatcher>,
    pub routes: Vec<Route>,
}

impl VirtualHost {
    fn from_xds(xds: &xds_route::VirtualHost) -> Result<Self, ResourceError> {
        // domains can't be empty and has to be a valid set of matchers.
        if xds.domains.is_empty() {
            return Err(ResourceError::invalid("empty domain list").with_field("domains"))?;
        }
        let mut domains = Vec::with_capacity(xds.domains.len());
        for (i, domain) in xds.domains.iter().enumerate() {
            domains.push(DomainMatcher::from_str(domain).with_field_index("domains", i)?);
        }

        // descend for routes
        let routes = vec_from_xds!(xds.routes, "routes", Route)?;
        Ok(Self { domains, routes })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Route {
    pub matcher: Matcher,
    pub action: Action,
}

impl Route {
    fn from_xds(xds: &xds_route::Route) -> Result<Self, ResourceError> {
        let matcher = xds
            .r#match
            .as_ref()
            .ok_or_else(|| ResourceError::invalid("RouteMatch is required"))
            .with_field("match")?;
        let matcher = Matcher::from_xds(matcher).with_field("match")?;

        let action = xds
            .action
            .as_ref()
            .ok_or_else(|| ResourceError::invalid("missing route action"))
            .with_field("action")?;
        let action = Action::from_xds(action).with_field("action")?;

        Ok(Self { matcher, action })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Action {
    pub hash_policies: Vec<HashPolicy>,
    pub retries: Option<Retries>,
    pub timeouts: Option<Timeouts>,
    pub cluster: ClusterSpecifier,
}

impl Action {
    fn from_xds(xds: &xds_route::route::Action) -> Result<Self, ResourceError> {
        match xds {
            xds_route::route::Action::Route(action) => {
                let cluster = match &action.cluster_specifier {
                    Some(xds_route::route_action::ClusterSpecifier::Cluster(cluster)) => {
                        ClusterSpecifier::Cluster(ResourceName::from(cluster.to_string()))
                    }
                    Some(xds_route::route_action::ClusterSpecifier::WeightedClusters(_)) => todo!(),
                    Some(_) => todo!(),
                    None => todo!(),
                };
                let hash_policies = vec_from_xds!(action.hash_policy, "hash_policy", HashPolicy)?;
                let retries = Retries::from_xds(&action)?;
                let timeouts = Timeouts::from_xds(&action)?;

                Ok(Self {
                    hash_policies,
                    cluster,
                    retries,
                    timeouts,
                })
            }
            _ => return Err(ResourceError::invalid("unsupported action")),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ClusterSpecifier {
    Cluster(ResourceName),
    Weighted(Vec<ClusterWeight>),
}

#[derive(Debug, Clone)]
pub(crate) struct ClusterWeight {
    pub name: ResourceName,
    pub weight: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct HashPolicy {
    pub terminal: bool,
    pub hasher: RequestHasher,
}

impl HashPolicy {
    fn from_xds(xds: &xds_route::route_action::HashPolicy) -> Result<Self, ResourceError> {
        let hasher = match &xds.policy_specifier {
            Some(xds_route::route_action::hash_policy::PolicySpecifier::Header(xds)) => {
                Ok(RequestHasher::Header {
                    name: xds.header_name.clone(),
                })
            }
            Some(xds_route::route_action::hash_policy::PolicySpecifier::QueryParameter(xds)) => {
                Ok(RequestHasher::QueryParameter {
                    name: xds.name.clone(),
                })
            }
            Some(_) => Err(ResourceError::invalid("unsupported policy")),
            None => Err(ResourceError::invalid("missing policiy specifier")),
        };

        let terminal = xds.terminal;
        let hasher = hasher.with_field("policy_specifier")?;

        Ok(Self { terminal, hasher })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum RequestHasher {
    Header { name: String },
    QueryParameter { name: String },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Retries {
    pub(crate) codes: Vec<u16>,
    pub(crate) attempts: Option<u32>,
    pub(crate) backoff: Option<Duration>,
    pub(crate) max_backoff: Option<Duration>,
}

impl Retries {
    fn from_xds(xds: &xds_route::RouteAction) -> Result<Option<Self>, ResourceError> {
        let Some(xds_retry) = xds.retry_policy.as_ref() else {
            return Ok(None);
        };

        let codes: Vec<_> = xds_retry
            .retriable_status_codes
            .iter()
            .map(|code| *code as u16)
            .collect();

        let attempts = xds_retry.num_retries.map(|v| u32::from(v) + 1);
        let (backoff, max_backoff) = match &xds_retry.retry_back_off {
            Some(back_off) => {
                let backoff = back_off
                    .base_interval
                    .map(Duration::try_from)
                    .transpose()
                    .map_err(|_| ResourceError::invalid("invalid duration"))
                    .with_fields("retry_policy", "retry_back_off")?;

                let max_backoff = back_off
                    .max_interval
                    .map(Duration::try_from)
                    .transpose()
                    .map_err(|_| ResourceError::invalid("invalid duration"))
                    .with_fields("retry_policy", "max_interval")?;

                (backoff, max_backoff)
            }
            None => (None, None),
        };

        let retries = match (&codes, attempts, backoff, max_backoff) {
            (v, None, None, None) if v.is_empty() => None,
            _ => Some(Self {
                codes,
                attempts,
                backoff,
                max_backoff,
            }),
        };
        Ok(retries)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Timeouts {
    pub(crate) total: Option<Duration>,
    pub(crate) attempt: Option<Duration>,
}

impl Timeouts {
    fn from_xds(xds: &xds_route::RouteAction) -> Result<Option<Self>, ResourceError> {
        let total = xds
            .timeout
            .map(Duration::try_from)
            .transpose()
            .map_err(|_| ResourceError::invalid("invalid duration"))
            .with_field("timeout")?;

        let attempt = xds
            .retry_policy
            .as_ref()
            .and_then(|retry_policy| retry_policy.per_try_timeout.map(Duration::try_from))
            .transpose()
            .map_err(|_| ResourceError::invalid("invalid duration"))
            .with_fields("retry_policy", "per_try_timeout")?;

        match (total, attempt) {
            (None, None) => Ok(None),
            (total, attempt) => Ok(Some(Self { total, attempt })),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Matcher {
    pub path: PathMatcher,
    pub method: Option<MethodMatcher>,
    pub headers: Vec<HeaderMatcher>,
    pub query: Vec<QueryMatcher>,
}

impl Matcher {
    fn from_xds(xds: &xds_route::RouteMatch) -> Result<Self, ResourceError> {
        let path = xds
            .path_specifier
            .as_ref()
            .ok_or_else(|| ResourceError::invalid("missing path specifier"))
            .and_then(|p| PathMatcher::from_xds(p))
            .with_field("path_specifier")?;

        Ok(Matcher {
            path,
            method: None,
            headers: vec![],
            query: vec![],
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum DomainMatcher {
    Exact(Hostname),
    Subdomain(Hostname),
}

impl DomainMatcher {
    pub(crate) fn matches_hostname(&self, s: &str) -> bool {
        match self {
            Self::Subdomain(d) => {
                let (subdomain, domain) = s.split_at(s.len() - d.len());
                domain == &d[..] && subdomain.ends_with('.')
            }
            Self::Exact(e) => s == e.as_ref(),
        }
    }
}

impl FromStr for DomainMatcher {
    type Err = ResourceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.strip_prefix("*.") {
            Some(hostname) => Self::Subdomain(
                Hostname::from_str(hostname)
                    .map_err(|_| ResourceError::invalid("invalid hostname"))?,
            ),
            None => Self::Exact(
                Hostname::from_str(s).map_err(|_| ResourceError::invalid("invalid hostname"))?,
            ),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MethodMatcher {
    method: http::Method,
}

impl MethodMatcher {
    pub(crate) fn is_match(&self, method: &http::Method) -> bool {
        self.method == method
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PathMatcher(StringMatcher);

impl PathMatcher {
    pub(crate) fn matches_path(&self, path: &str) -> bool {
        self.0.matches(path)
    }

    fn from_xds(xds: &xds_route::route_match::PathSpecifier) -> Result<Self, ResourceError> {
        let matcher = match &xds {
            xds_route::route_match::PathSpecifier::Prefix(p) => {
                StringMatcher::Prefix { value: p.clone() }
            }
            xds_route::route_match::PathSpecifier::Path(p) => {
                StringMatcher::Exact { value: p.clone() }
            }
            xds_route::route_match::PathSpecifier::SafeRegex(p) => {
                StringMatcher::RegularExpression {
                    value: parse_xds_regex(p).with_field("safe_regex")?,
                }
            }
            _ => return Err(ResourceError::invalid("unsupported match type")),
        };
        Ok(Self(matcher))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct HeaderMatcher {
    pub name: String,
    invert: bool,
    matcher: Option<StringMatcher>,
}

impl HeaderMatcher {
    pub(crate) fn matches_value(&self, header_value: Option<&[u8]>) -> bool {
        let matches = match &self.matcher {
            Some(matcher) => {
                // try to parse the header value as ascii, using the empty
                // string as the incoming value if it's not present.
                //
                // https://www.rfc-editor.org/rfc/rfc7230#section-3.2.6
                let Some(header_value) = from_ascii(header_value.unwrap_or_default()) else {
                    return false;
                };
                matcher.matches(header_value)
            }
            None => header_value.is_some(),
        };
        // if invert is true, we need to flip the conditional
        matches && !self.invert
    }
}

#[inline]
fn from_ascii(bs: &[u8]) -> Option<&str> {
    if !bs.is_ascii() {
        return None;
    }
    Some(str::from_utf8(bs).expect("expected a valid ascii string"))
}

#[derive(Debug, Clone)]
pub(crate) struct QueryMatcher {
    pub name: String,
    matcher: Option<StringMatcher>,
}

impl QueryMatcher {
    pub(crate) fn matches(&self, s: Option<&str>) -> bool {
        match &self.matcher {
            Some(matcher) => matcher.matches(s.unwrap_or_default()),
            None => s.is_some(),
        }
    }
}

#[derive(Debug, Clone)]
enum StringMatcher {
    Prefix { value: String },
    Suffix { value: String },
    RegularExpression { value: Regex },
    Exact { value: String },
}

impl StringMatcher {
    fn matches(&self, s: &str) -> bool {
        match self {
            StringMatcher::Prefix { value } => s.starts_with(value),
            StringMatcher::Suffix { value } => s.ends_with(value),
            Self::RegularExpression { value } => value.is_match(s),
            Self::Exact { value } => value == s,
        }
    }
}

fn parse_xds_regex(
    p: &xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher,
) -> Result<Regex, ResourceError> {
    Regex::from_str(&p.regex)
        .map_err(|e| ResourceError::invalid_with(format!("invalid regex: {e}")))
}
