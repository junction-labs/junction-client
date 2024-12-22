use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    str::FromStr,
};

use crate::{
    error::{Error, ErrorContext},
    http::{
        BackendRef, HeaderMatch, Method, PathMatch, QueryParamMatch, Route, RouteMatch, RouteRetry,
        RouteRule, RouteTimeouts,
    },
    shared::{Duration, Regex},
    Name,
};
use xds_api::pb::{
    envoy::{
        config::{
            core::v3 as xds_core,
            route::v3::{self as xds_route, query_parameter_matcher::QueryParameterMatchSpecifier},
        },
        r#type::matcher::v3::{string_matcher::MatchPattern, StringMatcher},
    },
    google::{
        self,
        protobuf::{self, UInt32Value},
    },
};

use crate::xds::shared::{parse_xds_regex, regex_matcher};

impl TryInto<Route> for &xds_route::RouteConfiguration {
    type Error = Error;

    fn try_into(self) -> Result<Route, Self::Error> {
        Route::from_xds(self)
    }
}

impl From<&Route> for xds_route::RouteConfiguration {
    fn from(route: &Route) -> Self {
        route.to_xds()
    }
}

impl Route {
    pub fn from_xds(xds: &xds_route::RouteConfiguration) -> Result<Self, Error> {
        let id = Name::from_str(&xds.name).with_field("name")?;
        let tags = tags_from_xds(&xds.metadata)?;

        let [vhost] = &xds.virtual_hosts[..] else {
            return Err(Error::new_static(
                "RouteConfiguration must have exactly one VirtualHost",
            ));
        };

        let mut hostnames = Vec::new();
        let mut ports = Vec::new();
        for (i, domain) in vhost.domains.iter().enumerate() {
            let (name, port) = crate::parse_port(domain).with_field_index("domains", i)?;

            let name = crate::http::HostnameMatch::from_str(name)
                .with_field_index("domains", i)
                .with_field_index("virtual_hosts", 0)?;

            hostnames.push(name);
            if let Some(port) = port {
                ports.push(port);
            }
        }
        hostnames.sort();
        hostnames.dedup();
        ports.sort();
        ports.dedup();

        let mut rules = vec![];
        let actions_and_matches = vhost.routes.iter().enumerate().map(|(route_idx, route)| {
            let key = (route.name.as_str(), route.action.as_ref());
            (key, (route_idx, route))
        });

        for ((name, action), matches) in group_by(actions_and_matches) {
            // safety: group_by shouldn't be able to emit an empty group
            let action_idx = matches
                .first()
                .map(|(idx, _)| *idx)
                .expect("missing route index");

            let Some(action) = &action else {
                return Err(Error::new_static("route has no route action"))
                    .with_field_index("routes", action_idx)
                    .with_field_index("virtual_hosts", 0);
            };

            rules.push(
                RouteRule::from_xds_grouped(name, action, &matches)
                    .with_field_index("virtual_hosts", 0)?,
            );
        }

        Ok(Route {
            id,
            hostnames,
            ports,
            tags,
            rules,
        })
    }

    pub fn to_xds(&self) -> xds_route::RouteConfiguration {
        let routes = self.rules.iter().flat_map(|rule| rule.to_xds()).collect();

        let mut domains = Vec::with_capacity(self.hostnames.len() * self.ports.len().min(1));
        for hostname in &self.hostnames {
            if self.ports.is_empty() {
                domains.push(hostname.to_string());
            } else {
                for port in &self.ports {
                    domains.push(format!("{hostname}:{port}"));
                }
            }
        }

        let virtual_hosts = vec![xds_route::VirtualHost {
            domains,
            routes,
            ..Default::default()
        }];

        let name = self.id.to_string();
        let metadata = tags_to_xds(&self.tags);

        xds_route::RouteConfiguration {
            name,
            metadata,
            virtual_hosts,
            ..Default::default()
        }
    }
}

const JUNCTION_ROUTE_TAGS: &str = "io.junctionlabs.route.tags";

fn tags_to_xds(tags: &BTreeMap<String, String>) -> Option<xds_core::Metadata> {
    if tags.is_empty() {
        return None;
    }

    let fields: HashMap<_, _> = tags
        .iter()
        .map(|(k, v)| {
            let v = protobuf::Value {
                kind: Some(protobuf::value::Kind::StringValue(v.clone())),
            };
            (k.clone(), v)
        })
        .collect();

    let mut metadata = xds_core::Metadata::default();
    metadata
        .filter_metadata
        .insert(JUNCTION_ROUTE_TAGS.to_string(), protobuf::Struct { fields });

    Some(metadata)
}

fn tags_from_xds(metadata: &Option<xds_core::Metadata>) -> Result<BTreeMap<String, String>, Error> {
    let Some(metadata) = metadata else {
        return Ok(Default::default());
    };

    let Some(route_tags) = metadata.filter_metadata.get(JUNCTION_ROUTE_TAGS) else {
        return Ok(Default::default());
    };

    let mut tags = BTreeMap::new();
    for (k, v) in route_tags.fields.iter() {
        let v = match &v.kind {
            Some(protobuf::value::Kind::StringValue(v)) => v.clone(),
            _ => {
                return Err(Error::new_static("invalid tag"))
                    .with_fields("filter_metadata", JUNCTION_ROUTE_TAGS)
            }
        };

        tags.insert(k.clone(), v);
    }

    Ok(tags)
}

impl RouteRule {
    fn from_xds_grouped(
        name: &str,
        action: &xds_route::route::Action,
        routes: &[(usize, &xds_route::Route)],
    ) -> Result<Self, Error> {
        let mut matches = vec![];
        for (route_idx, route) in routes {
            if let Some(route_match) = &route.r#match {
                let m = RouteMatch::from_xds(route_match)
                    .with_field("match")
                    .with_field_index("route", *route_idx)?;
                matches.push(m);
            }
        }

        // grab the index of the first xds Route to use in errors below. we're
        // assuming that there will always be a non-empty vec of route_matches
        // here.
        let first_route_idx = routes
            .first()
            .map(|(idx, _)| *idx)
            .expect("no Routes grouped with action. this is a bug in Junction");

        // parse a route name into aa validated Name. also assume they're all the same.
        let name = if !name.is_empty() {
            Some(
                Name::from_str(name)
                    .with_field("name")
                    .with_field_index("route", first_route_idx)?,
            )
        } else {
            None
        };

        let action = match action {
            xds_route::route::Action::Route(action) => action,
            _ => {
                return Err(Error::new_static("unsupported route action").with_field("action"))
                    .with_field_index("route", first_route_idx)
            }
        };

        let timeouts = RouteTimeouts::from_xds(action)?;
        let retry = action.retry_policy.as_ref().map(RouteRetry::from_xds);
        // cluster_specifier is a oneof field, so let WeightedTarget specify the
        // field names in errors. we still need to get the index to the
        let backends = BackendRef::from_xds(action.cluster_specifier.as_ref())
            .with_field("action")
            .with_field_index("route", first_route_idx)?;

        Ok(RouteRule {
            name,
            matches,
            retry,
            filters: vec![],
            timeouts,
            backends,
        })
    }

    pub fn to_xds(&self) -> Vec<xds_route::Route> {
        // retry policy
        let mut retry_policy = self.retry.as_ref().map(RouteRetry::to_xds);

        // timeouts
        //
        // the overall timeout gets set on the route action and the per-try
        // timeout gets set on the policy itself. have to create a default
        // policy if we don't have one already.
        let (timeout, per_try_timeout) = self
            .timeouts
            .as_ref()
            .map(RouteTimeouts::to_xds)
            .unwrap_or((None, None));

        if let Some(per_try_timeout) = per_try_timeout {
            retry_policy
                .get_or_insert_with(Default::default)
                .per_try_timeout = Some(per_try_timeout);
        }

        let cluster_specifier = BackendRef::to_xds(&self.backends);

        // tie it all together into a route action that we can use for each match
        let route_action = xds_route::route::Action::Route(xds_route::RouteAction {
            timeout,
            retry_policy,
            cluster_specifier,
            ..Default::default()
        });

        let name = self
            .name
            .as_ref()
            .map(|name| name.to_string())
            .unwrap_or_default();

        if self.matches.is_empty() {
            vec![xds_route::Route {
                name,
                r#match: Some(xds_route::RouteMatch {
                    path_specifier: Some(xds_route::route_match::PathSpecifier::Prefix(
                        "".to_string(),
                    )),
                    ..Default::default()
                }),
                action: Some(route_action),
                ..Default::default()
            }]
        } else {
            self.matches
                .iter()
                .map(|route_match| {
                    let r#match = Some(route_match.to_xds());
                    xds_route::Route {
                        name: name.clone(),
                        r#match,
                        action: Some(route_action.clone()),
                        ..Default::default()
                    }
                })
                .collect()
        }
    }
}

impl RouteTimeouts {
    pub fn from_xds(r: &xds_route::RouteAction) -> Result<Option<Self>, Error> {
        let request = r.timeout.clone().map(Duration::try_from).transpose()?;
        let backend_request = r
            .retry_policy
            .as_ref()
            .and_then(|retry_policy| retry_policy.per_try_timeout.clone().map(Duration::try_from))
            .transpose()?;

        if request.is_some() || backend_request.is_some() {
            Ok(Some(RouteTimeouts {
                request,
                backend_request,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn to_xds(
        &self,
    ) -> (
        Option<google::protobuf::Duration>,
        Option<google::protobuf::Duration>,
    ) {
        let request_timeout = self.request.map(|d| d.try_into().unwrap());
        let per_try_timeout = self.backend_request.map(|d| d.try_into().unwrap());
        (request_timeout, per_try_timeout)
    }
}

impl RouteMatch {
    pub fn from_xds(r: &xds_route::RouteMatch) -> Result<Self, Error> {
        // NOTE: because path_specifier is a oneof, each individual branch has
        // its own field name, so we don't add with_field(..) to the error.
        let path = r
            .path_specifier
            .as_ref()
            .map(PathMatch::from_xds)
            .transpose()?;

        // with xds, any method match is converted into a match on a ":method"
        // header. it does not have a way of specifying a method match
        // otherwise. to keep the "before and after" xDS as similar as possible
        // then we pull this out of the headers list if it exists
        let mut method: Option<Method> = None;
        let mut headers = vec![];
        for (i, header) in r.headers.iter().enumerate() {
            let header_match = HeaderMatch::from_xds(header).with_field_index("headers", i)?;

            match header_match {
                HeaderMatch::Exact { name, value } if name == ":method" => {
                    method = Some(value);
                }
                _ => {
                    headers.push(header_match);
                }
            }
        }

        // query parameters get their own field name, the oneof happens inside
        // the array of matches.
        let query_params = r
            .query_parameters
            .iter()
            .enumerate()
            .map(|(i, e)| QueryParamMatch::from_xds(e).with_field_index("query_parameters", i))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(RouteMatch {
            headers,
            method,
            path,
            query_params,
        })
    }

    fn to_xds(&self) -> xds_route::RouteMatch {
        // path
        let path_specifier = self.path.as_ref().map(|p| p.to_xds());

        let mut headers = vec![];
        // method
        if let Some(method) = &self.method {
            headers.push(xds_route::HeaderMatcher {
                name: ":method".to_string(),
                header_match_specifier: Some(
                    xds_route::header_matcher::HeaderMatchSpecifier::ExactMatch(method.to_string()),
                ),
                ..Default::default()
            })
        }
        // headers
        for header_match in &self.headers {
            headers.push(header_match.to_xds());
        }

        //query
        let query_parameters = self
            .query_params
            .iter()
            .map(QueryParamMatch::to_xds)
            .collect();

        xds_route::RouteMatch {
            headers,
            path_specifier,
            query_parameters,
            ..Default::default()
        }
    }
}

impl QueryParamMatch {
    pub fn from_xds(matcher: &xds_route::QueryParameterMatcher) -> Result<Self, Error> {
        let name = matcher.name.clone();
        match matcher.query_parameter_match_specifier.as_ref() {
            Some(QueryParameterMatchSpecifier::StringMatch(s)) => {
                let match_pattern = match s.match_pattern.as_ref() {
                    Some(MatchPattern::Exact(s)) => Ok(QueryParamMatch::Exact {
                        name,
                        value: s.clone(),
                    }),
                    Some(MatchPattern::SafeRegex(pfx)) => Ok(QueryParamMatch::RegularExpression {
                        name,
                        value: parse_xds_regex(pfx)?,
                    }),
                    Some(_) => Err(Error::new_static("unsupported string match type")),
                    None => Err(Error::new_static("missing string match")),
                };
                match_pattern.with_field("string_match")
            }
            Some(QueryParameterMatchSpecifier::PresentMatch(true)) => {
                Ok(QueryParamMatch::RegularExpression {
                    name,
                    value: Regex::from_str(".*").unwrap(),
                })
            }
            Some(QueryParameterMatchSpecifier::PresentMatch(false)) => {
                Err(Error::new_static("absent matches are not supported")
                    .with_field("present_match"))
            }
            // this isn't specified in the documentation, but the envoy code seems to
            // tolerate a missing value here.
            //
            // https://github.com/envoyproxy/envoy/blob/main/source/common/router/config_utility.cc#L18-L57
            None => Ok(QueryParamMatch::RegularExpression {
                name,
                value: Regex::from_str(".*").unwrap(),
            }),
        }
    }

    pub fn to_xds(&self) -> xds_route::QueryParameterMatcher {
        let (name, matcher) = match self {
            QueryParamMatch::RegularExpression { name, value } => {
                let name = name.clone();
                let matcher = MatchPattern::SafeRegex(regex_matcher(value));
                (name, matcher)
            }
            QueryParamMatch::Exact { name, value } => {
                let name = name.clone();
                let matcher = MatchPattern::Exact(value.to_string());
                (name, matcher)
            }
        };

        xds_route::QueryParameterMatcher {
            name,
            query_parameter_match_specifier: Some(QueryParameterMatchSpecifier::StringMatch(
                StringMatcher {
                    match_pattern: Some(matcher),
                    ignore_case: false,
                },
            )),
        }
    }
}

impl HeaderMatch {
    fn from_xds(header_matcher: &xds_route::HeaderMatcher) -> Result<Self, Error> {
        use xds_route::header_matcher::HeaderMatchSpecifier;

        let name = header_matcher.name.clone();
        match header_matcher.header_match_specifier.as_ref() {
            Some(HeaderMatchSpecifier::ExactMatch(value)) => Ok(HeaderMatch::Exact {
                name,
                value: value.clone(),
            }),
            Some(HeaderMatchSpecifier::SafeRegexMatch(regex)) => {
                Ok(HeaderMatch::RegularExpression {
                    name,
                    value: parse_xds_regex(regex)?,
                })
            }
            // we can support present matches but not absent matches
            Some(HeaderMatchSpecifier::PresentMatch(true)) => Ok(HeaderMatch::RegularExpression {
                name,
                value: Regex::from_str(".*").unwrap(),
            }),
            Some(_) => Err(Error::new_static("unsupported matcher")),
            None => Ok(HeaderMatch::RegularExpression {
                name,
                value: Regex::from_str(".*").unwrap(),
            }),
        }
    }

    fn to_xds(&self) -> xds_route::HeaderMatcher {
        match self {
            HeaderMatch::RegularExpression { name, value } => xds_route::HeaderMatcher {
                name: name.clone(),
                header_match_specifier: Some(
                    xds_route::header_matcher::HeaderMatchSpecifier::SafeRegexMatch(regex_matcher(
                        value,
                    )),
                ),
                ..Default::default()
            },
            HeaderMatch::Exact { name, value } => xds_route::HeaderMatcher {
                name: name.clone(),
                header_match_specifier: Some(
                    xds_route::header_matcher::HeaderMatchSpecifier::ExactMatch(value.to_string()),
                ),
                ..Default::default()
            },
        }
    }
}

impl PathMatch {
    fn from_xds(path_spec: &xds_route::route_match::PathSpecifier) -> Result<Self, Error> {
        match path_spec {
            xds_route::route_match::PathSpecifier::Prefix(p) => {
                Ok(PathMatch::Prefix { value: p.clone() })
            }
            xds_route::route_match::PathSpecifier::Path(p) => {
                Ok(PathMatch::Exact { value: p.clone() })
            }
            xds_route::route_match::PathSpecifier::SafeRegex(p) => {
                Ok(PathMatch::RegularExpression {
                    value: parse_xds_regex(p).with_field("safe_regex")?,
                })
            }
            _ => Err(Error::new_static("unsupported path specifier")),
        }
    }

    pub fn to_xds(&self) -> xds_route::route_match::PathSpecifier {
        match self {
            PathMatch::Prefix { value } => {
                xds_route::route_match::PathSpecifier::Prefix(value.to_string())
            }
            PathMatch::RegularExpression { value } => {
                xds_route::route_match::PathSpecifier::SafeRegex(regex_matcher(value))
            }
            PathMatch::Exact { value } => {
                xds_route::route_match::PathSpecifier::Path(value.clone())
            }
        }
    }
}

impl RouteRetry {
    pub fn from_xds(r: &xds_route::RetryPolicy) -> Self {
        let codes = r
            .retriable_status_codes
            .iter()
            .map(|code| *code as u16)
            .collect();
        let attempts = r.num_retries.clone().map(|v| u32::from(v) + 1);
        let backoff = r
            .retry_back_off
            .as_ref()
            .and_then(|r2| r2.base_interval.clone().map(|x| x.try_into().unwrap()));
        Self {
            codes,
            attempts,
            backoff,
        }
    }

    pub fn to_xds(&self) -> xds_route::RetryPolicy {
        let retriable_status_codes = self.codes.iter().map(|&code| code as u32).collect();
        let num_retries = self
            .attempts
            .map(|attempts| UInt32Value::from(attempts.saturating_sub(1)));

        let retry_back_off = self.backoff.map(|b| xds_route::retry_policy::RetryBackOff {
            base_interval: Some(b.try_into().unwrap()),
            max_interval: None,
        });

        xds_route::RetryPolicy {
            retriable_status_codes,
            num_retries,
            retry_back_off,
            ..Default::default()
        }
    }
}

/// Group an iterator of `(k, v)` pairs together by `k`. Returns an iterator
/// over `(K, Vec<V>)` pairs.
///
/// Like `uniq`, only groups consecutively unique items together.
///
/// `group_by` should never emit an item with an empty Vec - to have a key,
/// there must have also been a value.
fn group_by<I, K, V>(iter: I) -> GroupBy<<I as IntoIterator>::IntoIter, K, V>
where
    I: IntoIterator<Item = (K, V)>,
    K: PartialEq,
{
    GroupBy {
        iter: iter.into_iter(),
        current_key: None,
        current_values: Vec::new(),
    }
}

struct GroupBy<I, K, V> {
    iter: I,
    current_key: Option<K>,
    current_values: Vec<V>,
}

impl<I, K, V> Iterator for GroupBy<I, K, V>
where
    I: Iterator<Item = (K, V)>,
    K: PartialEq + Debug,
    V: Debug,
{
    type Item = (K, Vec<V>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match (self.current_key.take(), self.iter.next()) {
                // data, no previous group
                (None, Some((k, v))) => {
                    self.current_key = Some(k);
                    self.current_values.push(v);
                }
                // add to the previous group
                (Some(current_key), Some((next_key, v))) if next_key == current_key => {
                    self.current_key = Some(current_key);
                    self.current_values.push(v)
                }
                // emit the previous group, add a new group
                (Some(current_key), Some((next_key, v))) => {
                    let values = std::mem::take(&mut self.current_values);

                    // save the next key and value
                    self.current_key = Some(next_key);
                    self.current_values.push(v);

                    return Some((current_key, values));
                }
                // the iterator is done, but the last group hasn't been emitted.
                (Some(key), None) => {
                    let values = std::mem::take(&mut self.current_values);
                    return Some((key, values));
                }
                // everyone is done
                (None, None) => return None,
            }
        }
    }
}

impl BackendRef {
    pub(crate) fn to_xds(wbs: &[Self]) -> Option<xds_route::route_action::ClusterSpecifier> {
        match wbs {
            [] => None,
            [backend] => Some(xds_route::route_action::ClusterSpecifier::Cluster(
                backend.name(),
            )),
            targets => {
                let clusters = targets
                    .iter()
                    .map(|wb| xds_route::weighted_cluster::ClusterWeight {
                        name: wb.name(),
                        weight: Some(wb.weight.into()),
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
            Some(xds_route::route_action::ClusterSpecifier::Cluster(name)) => {
                BackendRef::from_str(name)
                    .map(|br| vec![br])
                    .with_field("cluster")
            }
            Some(xds_route::route_action::ClusterSpecifier::WeightedClusters(
                weighted_clusters,
            )) => {
                let clusters = weighted_clusters.clusters.iter().enumerate().map(|(i, w)| {
                    let backend_ref = BackendRef::from_str(&w.name).with_field_index("name", i)?;
                    let weight = crate::value_or_default!(w.weight, 1);
                    Ok(Self {
                        weight,
                        ..backend_ref
                    })
                });

                clusters
                    .collect::<Result<Vec<_>, _>>()
                    .with_fields("weighted_clusters", "clusters")
            }
            Some(_) => Err(Error::new_static("unsupported cluster specifier")),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{http::HostnameMatch, Hostname, Service};

    #[test]
    fn test_group_by() {
        let groups: Vec<_> = group_by([(1, "a"), (2, "b"), (3, "c")]).collect();
        assert_eq!(vec![(1, vec!["a"]), (2, vec!["b"]), (3, vec!["c"])], groups);

        let groups: Vec<_> = group_by([
            (1, "a"),
            (1, "a"),
            (2, "b"),
            (3, "c"),
            (3, "c"),
            (3, "c"),
            (1, "a"),
        ])
        .collect();
        assert_eq!(
            vec![
                (1, vec!["a", "a"]),
                (2, vec!["b"]),
                (3, vec!["c", "c", "c"]),
                (1, vec!["a"]),
            ],
            groups
        );
    }

    #[test]
    fn test_simple_route() {
        let web = Service::kube("prod", "web").unwrap();

        let original = Route {
            id: Name::from_static("my-route"),
            hostnames: vec![web.hostname().into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                backends: vec![BackendRef {
                    weight: 1,
                    service: web.clone(),
                    port: None,
                }],
                ..Default::default()
            }],
        };

        let round_tripped = Route::from_xds(&original.to_xds()).unwrap();
        let expected = Route {
            id: Name::from_static("my-route"),
            hostnames: vec![web.hostname().into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::empty_prefix()),
                    ..Default::default()
                }],
                backends: vec![BackendRef {
                    weight: 1,
                    service: web.clone(),
                    port: None,
                }],
                ..Default::default()
            }],
        };
        assert_eq!(round_tripped, expected)
    }

    #[test]
    fn test_wildcard_hostname() {
        let web = Service::kube("prod", "web").unwrap();

        let original = Route {
            id: Name::from_static("my-route"),
            hostnames: vec![
                HostnameMatch::from_str("*.prod.web.svc.cluster.local").unwrap(),
                HostnameMatch::from_str("*.staging.web.svc.cluster.local").unwrap(),
            ],
            ports: vec![80, 443],
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::empty_prefix()),
                    ..Default::default()
                }],
                backends: vec![BackendRef {
                    weight: 1,
                    service: web.clone(),
                    port: None,
                }],
                ..Default::default()
            }],
        };

        let round_tripped = Route::from_xds(&original.to_xds()).unwrap();
        assert_eq!(round_tripped, original)
    }

    #[test]
    fn test_route_no_rules() {
        let original = Route {
            id: Name::from_static("no-rules"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![],
        };

        let round_tripped = Route::from_xds(&original.to_xds()).unwrap();
        assert_eq!(round_tripped, original)
    }

    #[test]
    fn test_route_rule_no_backend() {
        let original = Route {
            id: Name::from_static("no-backends"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule::default()],
        };
        let normalized = Route {
            id: Name::from_static("no-backends"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::Prefix {
                        value: "".to_string(),
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let round_tripped = Route::from_xds(&original.to_xds()).unwrap();
        assert_eq!(round_tripped, normalized)
    }

    #[test]
    fn test_metadata_roundtrip() {
        let web = Service::kube("prod", "web").unwrap();

        assert_roundtrip::<_, xds_route::RouteConfiguration>(Route {
            id: Name::from_static("metadata"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: BTreeMap::from_iter([("foo".to_string(), "bar".to_string())]),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::Prefix {
                        value: "".to_string(),
                    }),
                    ..Default::default()
                }],
                backends: vec![BackendRef {
                    weight: 1,
                    service: web.clone(),
                    port: Some(8778),
                }],
                ..Default::default()
            }],
        });
    }

    #[test]
    fn test_multiple_rules_roundtrip() {
        let web = Service::kube("prod", "web").unwrap();
        let staging = Service::kube("staging", "web").unwrap();

        // should roundtrip with different targets
        assert_roundtrip::<_, xds_route::RouteConfiguration>(Route {
            id: Name::from_static("multiple-targets"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    name: Some(Name::from_static("split-web")),
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Exact {
                            value: "/foo/feature-test".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![
                        BackendRef {
                            weight: 3,
                            service: staging.clone(),
                            port: Some(80),
                        },
                        BackendRef {
                            weight: 1,
                            service: web.clone(),
                            port: Some(80),
                        },
                    ],
                    ..Default::default()
                },
                RouteRule {
                    name: Some(Name::from_static("one-web")),
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/foo".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: Some(80),
                    }],
                    ..Default::default()
                },
            ],
        });

        // should roundtrip with the same backends but different timeouts
        assert_roundtrip::<_, xds_route::RouteConfiguration>(Route {
            id: Name::from_static("same-target-multiple-timeouts"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    name: Some(Name::from_static("no-timeouts")),
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Exact {
                            value: "/foo/feature-test".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: None,
                    }],
                    ..Default::default()
                },
                RouteRule {
                    name: Some(Name::from_static("with-timeouts")),
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/foo".to_string(),
                        }),
                        ..Default::default()
                    }],
                    timeouts: Some(RouteTimeouts {
                        request: Some(Duration::from_secs(123)),
                        backend_request: None,
                    }),
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: None,
                    }],
                    ..Default::default()
                },
            ],
        });

        // should roundtrip with the same backends but different retries
        assert_roundtrip::<_, xds_route::RouteConfiguration>(Route {
            id: Name::from_static("same-target-multiple-retries"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Exact {
                            value: "/foo/feature-test".to_string(),
                        }),
                        ..Default::default()
                    }],
                    retry: Some(RouteRetry {
                        codes: vec![500, 503],
                        attempts: Some(123),
                        backoff: Some(Duration::from_secs(1)),
                    }),
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: None,
                    }],
                    ..Default::default()
                },
                RouteRule {
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/foo".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: None,
                    }],
                    ..Default::default()
                },
            ],
        });

        // should roundtrip with the same everything but different names
        assert_roundtrip::<_, xds_route::RouteConfiguration>(Route {
            id: Name::from_static("different-names"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    name: Some(Name::from_static("rule-1")),
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/foo".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: None,
                    }],
                    ..Default::default()
                },
                RouteRule {
                    name: Some(Name::from_static("rule-2")),
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/foo".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: None,
                    }],
                    ..Default::default()
                },
            ],
        });
    }

    #[test]
    fn test_condense_rules() {
        let web = Service::kube("prod", "web").unwrap();

        // should be condensed - the rules have the same everything except matches
        let original = Route {
            id: Name::from_static("will-be-condensed"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![
                RouteRule {
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Exact {
                            value: "/foo/feature-test".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: Some(80),
                    }],
                    ..Default::default()
                },
                RouteRule {
                    matches: vec![RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/foo".to_string(),
                        }),
                        ..Default::default()
                    }],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: Some(80),
                    }],
                    ..Default::default()
                },
            ],
        };

        // should condense rules if the backends and retries/etc are identical
        let converted = Route::from_xds(&original.to_xds()).unwrap();
        assert_eq!(
            converted,
            Route {
                id: Name::from_static("will-be-condensed"),
                hostnames: vec![Hostname::from_static("web.internal").into()],
                ports: vec![],
                tags: Default::default(),
                rules: vec![RouteRule {
                    matches: vec![
                        RouteMatch {
                            path: Some(PathMatch::Exact {
                                value: "/foo/feature-test".to_string(),
                            }),
                            ..Default::default()
                        },
                        RouteMatch {
                            path: Some(PathMatch::Prefix {
                                value: "/foo".to_string(),
                            }),
                            ..Default::default()
                        }
                    ],
                    backends: vec![BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: Some(80),
                    }],
                    ..Default::default()
                },],
            }
        )
    }

    #[test]
    fn test_multiple_matches_roundtrip() {
        let web = Service::kube("prod", "web").unwrap();

        assert_roundtrip::<_, xds_route::RouteConfiguration>(Route {
            id: Name::from_static("multiple-matches"),
            hostnames: vec![Hostname::from_static("web.internal").into()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![
                    RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/foo".to_string(),
                        }),
                        ..Default::default()
                    },
                    RouteMatch {
                        path: Some(PathMatch::Prefix {
                            value: "/bar".to_string(),
                        }),
                        ..Default::default()
                    },
                    RouteMatch {
                        query_params: vec![QueryParamMatch::Exact {
                            name: "param".to_string(),
                            value: "an_value".to_string(),
                        }],
                        ..Default::default()
                    },
                ],
                backends: vec![BackendRef {
                    weight: 1,
                    service: web.clone(),
                    port: Some(80),
                }],
                ..Default::default()
            }],
        });
    }

    #[test]
    fn test_full_route_match_roundtrips() {
        let web = Service::kube("prod", "web").unwrap();

        assert_roundtrip::<_, xds_route::RouteConfiguration>(Route {
            id: Name::from_static("full-send"),
            hostnames: vec![
                HostnameMatch::from_str("*.web.internal").unwrap(),
                Hostname::from_static("potato.tomato").into(),
                Hostname::from_static("web.internal").into(),
            ],
            ports: vec![80, 443, 8080],
            tags: [("foo".to_string(), "bar".to_string())]
                .into_iter()
                .collect(),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::Prefix {
                        value: "/potato".to_string(),
                    }),
                    headers: vec![HeaderMatch::RegularExpression {
                        name: "x-one".to_string(),
                        value: ".*".parse().unwrap(),
                    }],
                    query_params: vec![
                        QueryParamMatch::RegularExpression {
                            name: "foo".to_string(),
                            value: r"\w+".parse().unwrap(),
                        },
                        QueryParamMatch::Exact {
                            name: "bar".to_string(),
                            value: "baz".to_string(),
                        },
                    ],
                    method: Some("CONNECT".to_string()),
                }],
                backends: vec![
                    BackendRef {
                        weight: 1,
                        service: web.clone(),
                        port: Some(8080),
                    },
                    BackendRef {
                        weight: 0,
                        service: web.clone(),
                        port: None,
                    },
                ],
                ..Default::default()
            }],
        });
    }

    #[track_caller]
    fn assert_roundtrip<T, Xds>(v: T)
    where
        T: PartialEq + std::fmt::Debug,
        for<'a> &'a T: Into<Xds>,
        for<'a> &'a Xds: TryInto<T, Error = Error>,
    {
        let xds: Xds = (&v).into();
        let back: T = (&xds).try_into().unwrap();
        assert_eq!(v, back);
    }
}
