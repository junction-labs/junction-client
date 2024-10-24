use std::str::FromStr;

use gateway_api::apis::experimental::httproutes::{
    HTTPRoute, HTTPRouteParentRefs, HTTPRouteRules, HTTPRouteRulesBackendRefs,
    HTTPRouteRulesMatches, HTTPRouteRulesMatchesHeaders, HTTPRouteRulesMatchesHeadersType,
    HTTPRouteRulesMatchesMethod, HTTPRouteRulesMatchesPath, HTTPRouteRulesMatchesPathType,
    HTTPRouteRulesMatchesQueryParams, HTTPRouteRulesMatchesQueryParamsType, HTTPRouteRulesRetry,
    HTTPRouteRulesTimeouts, HTTPRouteSpec,
};
use kube::api::ObjectMeta;

use crate::error::{Error, ErrorContext};
use crate::shared::Regex;
use crate::{Duration, Hostname, Name};

impl crate::http::Route {
    /// Convert an [HTTPRouteSpec] into a [Route][crate::http::Route].
    #[inline]
    pub fn from_gateway_httproute(route_spec: &HTTPRouteSpec) -> Result<crate::http::Route, Error> {
        route_spec.try_into()
    }

    /// Convert this [Route][crate::http::Route] into a Gateway API
    /// [HTTPRouteSpec].
    #[inline]
    pub fn to_gateway_httproute_spec(&self) -> Result<HTTPRouteSpec, Error> {
        self.try_into()
    }

    /// Convert this [Route][crate::http::Route] into a Gateway API [HTTPRoute]
    /// with it's `name` and `namespace` metadata set.
    ///
    /// This is a convenience function for creating an [HTTPRoute] with a name
    /// you define. To create just an [HTTPRouteSpec] with other metadata, use
    /// [Self::to_gateway_httproute_spec].
    pub fn to_gateway_httproute(&self, namespace: &str, name: &str) -> Result<HTTPRoute, Error> {
        let spec = self.to_gateway_httproute_spec()?;
        Ok(HTTPRoute {
            metadata: ObjectMeta {
                namespace: Some(namespace.to_string()),
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec,
            status: None,
        })
    }
}

macro_rules! option_from_gateway {
    ($field:expr) => {
        $field.as_ref().map(|v| v.try_into()).transpose()
    };
}

macro_rules! vec_from_gateway {
    ($opt_vec:expr) => {
        $opt_vec
            .iter()
            .flatten()
            .enumerate()
            .map(|(i, e)| e.try_into().with_index(i))
            .collect::<Result<Vec<_>, _>>()
    };
}

macro_rules! option_to_gateway {
    ($field:expr) => {
        $field.as_ref().map(|e| e.try_into()).transpose()
    };
}

macro_rules! vec_to_gateway {
    ($vec:expr) => {
        $vec.iter()
            .enumerate()
            .map(|(i, e)| e.try_into().with_index(i))
            .collect::<Result<Vec<_>, _>>()
    };
}

// kube -> crate

impl TryFrom<&HTTPRouteSpec> for crate::http::Route {
    type Error = Error;

    fn try_from(spec: &HTTPRouteSpec) -> Result<Self, Error> {
        use crate::Target;

        // build a target from the parent ref. forbid having more than one parent ref.
        //
        // TOOD: we could allow converting one HTTPRoute into more than one Route
        let target = match spec.parent_refs.as_deref() {
            Some([parent_ref]) => Target::try_from(parent_ref).with_field_index("parentRefs", 0)?,
            Some(_) => {
                return Err(Error::new_static(
                    "HTTPRoute can't have more than one parent ref",
                ))
            }
            None => return Err(Error::new_static("HTTPRoute must have a parent ref")),
        };

        let rules = vec_from_gateway!(spec.rules).with_field("rules")?;
        Ok(Self { target, rules })
    }
}

impl TryFrom<&HTTPRouteRules> for crate::http::RouteRule {
    type Error = Error;
    fn try_from(rule: &HTTPRouteRules) -> Result<Self, Error> {
        let matches = vec_from_gateway!(rule.matches).with_field("matches")?;
        let timeouts = option_from_gateway!(rule.timeouts).with_field("timeouts")?;
        let backends = vec_from_gateway!(rule.backend_refs).with_field("backends")?;
        // FIXME: filters are ignored because they're not implemented yet
        let filters = vec![];
        let retry = option_from_gateway!(rule.retry).with_field("retry")?;

        Ok(crate::http::RouteRule {
            matches,
            filters,
            timeouts,
            retry,
            backends,
        })
    }
}

impl TryFrom<&HTTPRouteRulesTimeouts> for crate::http::RouteTimeouts {
    type Error = Error;
    fn try_from(timeouts: &HTTPRouteRulesTimeouts) -> Result<Self, Error> {
        let request = parse_duration(&timeouts.request).with_field("request")?;
        let backend_request =
            parse_duration(&timeouts.backend_request).with_field("backendRequest")?;

        Ok(crate::http::RouteTimeouts {
            request,
            backend_request,
        })
    }
}

impl TryFrom<&HTTPRouteRulesRetry> for crate::http::RouteRetry {
    type Error = Error;

    fn try_from(retry: &HTTPRouteRulesRetry) -> Result<Self, Self::Error> {
        let mut codes = Vec::with_capacity(retry.codes.as_ref().map_or(0, |c| c.len()));
        for (i, &code) in retry.codes.iter().flatten().enumerate() {
            let code: u32 = code
                .try_into()
                .map_err(|_| Error::new_static("invalid response code"))
                .with_field_index("codes", i)?;
            codes.push(code);
        }

        let attempts = retry
            .attempts
            .map(|i| i.try_into())
            .transpose()
            .map_err(|_| Error::new_static("invalid u32"))
            .with_field("attempts")?;

        let backoff = retry
            .backoff
            .as_ref()
            .map(|s| {
                Duration::from_str(s)
                    .map_err(Error::new)
                    .with_field("backoff")
            })
            .transpose()?;

        Ok(crate::http::RouteRetry {
            codes,
            attempts,
            backoff,
        })
    }
}

fn parse_duration(d: &Option<String>) -> Result<Option<crate::shared::Duration>, Error> {
    d.as_ref()
        .map(|d_str| {
            d_str
                .parse()
                .map_err(|e| Error::new(format!("invalid duration: {e}")))
        })
        .transpose()
}

impl TryFrom<&HTTPRouteRulesMatches> for crate::http::RouteMatch {
    type Error = Error;
    fn try_from(matches: &HTTPRouteRulesMatches) -> Result<Self, Error> {
        let method = matches
            .method
            .as_ref()
            .map(method_from_gateway)
            .transpose()
            .with_field("method")?;

        Ok(crate::http::RouteMatch {
            path: option_from_gateway!(matches.path).with_field("path")?,
            headers: vec_from_gateway!(matches.headers).with_field("headers")?,
            query_params: vec_from_gateway!(matches.query_params).with_field("queryParams")?,
            method,
        })
    }
}

impl TryFrom<&HTTPRouteRulesMatchesPath> for crate::http::PathMatch {
    type Error = Error;
    fn try_from(matches_path: &HTTPRouteRulesMatchesPath) -> Result<Self, Error> {
        use crate::http::PathMatch;

        let Some(value) = &matches_path.value else {
            return Err(Error::new_static("missing value"));
        };

        match matches_path.r#type {
            Some(HTTPRouteRulesMatchesPathType::Exact) => Ok(PathMatch::Exact {
                value: value.clone(),
            }),
            Some(HTTPRouteRulesMatchesPathType::PathPrefix) => Ok(PathMatch::Prefix {
                value: value.clone(),
            }),
            Some(HTTPRouteRulesMatchesPathType::RegularExpression) => {
                let value = Regex::from_str(value)
                    .map_err(|e| Error::new(format!("invalid regex: {e}")).with_field("value"))?;
                Ok(PathMatch::RegularExpression { value })
            }
            None => Err(Error::new_static("missing type")),
        }
    }
}

impl TryFrom<&HTTPRouteRulesMatchesHeaders> for crate::http::HeaderMatch {
    type Error = Error;
    fn try_from(matches_headers: &HTTPRouteRulesMatchesHeaders) -> Result<Self, Error> {
        use crate::http::HeaderMatch;

        let name = &matches_headers.name;
        let value = &matches_headers.value;
        match matches_headers.r#type {
            Some(HTTPRouteRulesMatchesHeadersType::Exact) => Ok(HeaderMatch::Exact {
                name: name.clone(),
                value: value.clone(),
            }),
            Some(HTTPRouteRulesMatchesHeadersType::RegularExpression) => {
                let value = Regex::from_str(value)
                    .map_err(|e| Error::new(format!("invalid regex: {e}")).with_field("value"))?;
                Ok(HeaderMatch::RegularExpression {
                    name: name.clone(),
                    value,
                })
            }
            None => Err(Error::new_static("missing type")),
        }
    }
}

impl TryFrom<&HTTPRouteRulesMatchesQueryParams> for crate::http::QueryParamMatch {
    type Error = Error;
    fn try_from(matches_query: &HTTPRouteRulesMatchesQueryParams) -> Result<Self, Error> {
        use crate::http::QueryParamMatch;

        let name = &matches_query.name;
        let value = &matches_query.value;
        match matches_query.r#type {
            Some(HTTPRouteRulesMatchesQueryParamsType::Exact) => Ok(QueryParamMatch::Exact {
                name: name.clone(),
                value: value.clone(),
            }),
            Some(HTTPRouteRulesMatchesQueryParamsType::RegularExpression) => {
                let value = Regex::from_str(value)
                    .map_err(|e| Error::new(format!("invalid regex: {e}")).with_field("value"))?;
                Ok(QueryParamMatch::RegularExpression {
                    name: name.clone(),
                    value,
                })
            }
            None => Err(Error::new_static("missing type")),
        }
    }
}

fn port_from_gateway(port: &Option<i32>) -> Result<Option<u16>, Error> {
    (*port)
        .map(|p| {
            p.try_into()
                .map_err(|_| Error::new_static("port value out of range"))
        })
        .transpose()
}

impl TryFrom<&HTTPRouteParentRefs> for crate::Target {
    type Error = Error;
    fn try_from(parent_ref: &HTTPRouteParentRefs) -> Result<Self, Error> {
        let group = parent_ref
            .group
            .as_deref()
            .unwrap_or("gateway.networking.k8s.io");

        match (group, parent_ref.kind.as_deref()) {
            ("junctionlabs.io", Some("DNS")) => Ok(crate::Target::DNS(crate::DNSTarget {
                hostname: Hostname::from_str(&parent_ref.name).with_field("name")?,
                port: port_from_gateway(&parent_ref.port).with_field("port")?,
            })),
            ("", Some("Service")) => {
                // NOTE: kube doesn't require the namespace, but we do for now.
                let namespace = parent_ref
                    .namespace
                    .as_deref()
                    .ok_or_else(|| Error::new_static("missing namespace"))
                    .and_then(Name::from_str)
                    .with_field("namespace")?;

                Ok(crate::Target::Service(crate::ServiceTarget {
                    name: Name::from_str(&parent_ref.name).with_field("name")?,
                    namespace,
                    port: port_from_gateway(&parent_ref.port).with_field("port")?,
                }))
            }
            (group, Some(kind)) => Err(Error::new(format!(
                "unsupported parent ref: {group}/{kind}"
            ))),
            _ => Err(Error::new("missing Kind".to_string())),
        }
    }
}

impl TryFrom<&HTTPRouteRulesBackendRefs> for crate::http::WeightedTarget {
    type Error = Error;
    fn try_from(backend_ref: &HTTPRouteRulesBackendRefs) -> Result<Self, Error> {
        let group = backend_ref.group.as_deref().unwrap_or("");
        let kind = backend_ref.kind.as_deref().unwrap_or("Service");
        let weight = backend_ref
            .weight
            .unwrap_or(1)
            .try_into()
            .map_err(|_| Error::new_static("negative weight"))
            .with_field("weight")?;

        let target = match (group, kind) {
            ("junctionlabs.io", "DNS") => {
                let port = port_from_gateway(&backend_ref.port).with_field("port")?;
                crate::Target::DNS(crate::DNSTarget {
                    hostname: Hostname::from_str(&backend_ref.name).with_field("name")?,
                    port,
                })
            }
            ("", "Service") => {
                let port = port_from_gateway(&backend_ref.port).with_field("port")?;
                // NOTE: kube doesn't require the namespace, but we do for now.
                let namespace = backend_ref
                    .namespace
                    .as_deref()
                    .ok_or_else(|| Error::new_static("missing namespace"))
                    .and_then(Name::from_str)
                    .with_field("namespace")?;

                crate::Target::Service(crate::ServiceTarget {
                    name: Name::from_str(&backend_ref.name).with_field("name")?,
                    port,
                    namespace,
                })
            }
            (group, kind) => {
                return Err(Error::new(format!(
                    "unsupported backend ref: {group}/{kind}"
                )))
            }
        };

        Ok(crate::http::WeightedTarget { weight, target })
    }
}

// liminal space, spooky

macro_rules! method_matches {
    ( $($method:ident => $str:expr,)* $(,)*) => {
        fn method_from_gateway(match_method: &HTTPRouteRulesMatchesMethod) -> Result<crate::http::Method, Error> {
            match match_method {
                $(
                    HTTPRouteRulesMatchesMethod::$method => Ok($str.to_string()),
                )*
            }
        }

        fn method_to_gateway(method: &crate::http::Method) -> Result<HTTPRouteRulesMatchesMethod, Error> {
            match method.as_str() {
                $(
                    $str => Ok(HTTPRouteRulesMatchesMethod::$method),
                )*
                _ => Err(Error::new(format!("unrecognized HTTP method: {}", method)))
            }
        }
    }
}

method_matches! {
    Get => "GET",
    Head => "HEAD",
    Post => "POST",
    Put => "PUT",
    Delete => "DELETE",
    Connect => "CONNECT",
    Options => "OPTIONS",
    Trace => "TRACE",
    Patch => "PATCH",
}

// crate -> kube

impl TryFrom<&crate::http::Route> for HTTPRouteSpec {
    type Error = Error;

    fn try_from(route: &crate::http::Route) -> Result<HTTPRouteSpec, Error> {
        let parent_ref = (&route.target).try_into().with_field("target")?;

        Ok(HTTPRouteSpec {
            hostnames: None,
            parent_refs: Some(vec![parent_ref]),
            rules: Some(vec_to_gateway!(route.rules).with_field("rules")?),
        })
    }
}

impl TryFrom<&crate::http::RouteRule> for HTTPRouteRules {
    type Error = Error;

    fn try_from(route_rule: &crate::http::RouteRule) -> Result<HTTPRouteRules, Error> {
        Ok(HTTPRouteRules {
            backend_refs: Some(vec_to_gateway!(route_rule.backends).with_field("backends")?),
            filters: None,
            matches: Some(vec_to_gateway!(route_rule.matches).with_field("matches")?),
            name: None,
            retry: option_to_gateway!(route_rule.retry).with_field("retry")?,
            session_persistence: None,
            timeouts: option_to_gateway!(route_rule.timeouts).with_field("timeouts")?,
        })
    }
}

impl TryFrom<&crate::http::RouteTimeouts> for HTTPRouteRulesTimeouts {
    type Error = Error;

    fn try_from(timeouts: &crate::http::RouteTimeouts) -> Result<HTTPRouteRulesTimeouts, Error> {
        Ok(HTTPRouteRulesTimeouts {
            backend_request: timeouts.backend_request.map(|d| d.to_string()),
            request: timeouts.request.map(|d| d.to_string()),
        })
    }
}

impl TryFrom<&crate::http::RouteRetry> for HTTPRouteRulesRetry {
    type Error = Error;

    fn try_from(retry: &crate::http::RouteRetry) -> Result<Self, Self::Error> {
        let attempts = retry.attempts.map(|n| n as i64);
        let backoff = retry.backoff.map(|d| d.to_string());
        let codes = if retry.codes.is_empty() {
            None
        } else {
            let codes = retry.codes.iter().map(|&code| code as i64).collect();
            Some(codes)
        };

        Ok(HTTPRouteRulesRetry {
            attempts,
            backoff,
            codes,
        })
    }
}

impl TryFrom<&crate::http::RouteMatch> for HTTPRouteRulesMatches {
    type Error = Error;

    fn try_from(route_match: &crate::http::RouteMatch) -> Result<HTTPRouteRulesMatches, Error> {
        let method = route_match
            .method
            .as_ref()
            .map(method_to_gateway)
            .transpose()
            .with_field("method")?;

        Ok(HTTPRouteRulesMatches {
            headers: Some(vec_to_gateway!(&route_match.headers).with_field("headers")?),
            method,
            path: option_to_gateway!(&route_match.path).with_field("path")?,
            query_params: Some(
                vec_to_gateway!(&route_match.query_params).with_field("query_params")?,
            ),
        })
    }
}

impl TryFrom<&crate::http::QueryParamMatch> for HTTPRouteRulesMatchesQueryParams {
    type Error = Error;
    fn try_from(
        query_match: &crate::http::QueryParamMatch,
    ) -> Result<HTTPRouteRulesMatchesQueryParams, Error> {
        Ok(match query_match {
            crate::http::QueryParamMatch::RegularExpression { name, value } => {
                HTTPRouteRulesMatchesQueryParams {
                    name: name.clone(),
                    r#type: Some(HTTPRouteRulesMatchesQueryParamsType::RegularExpression),
                    value: value.to_string(),
                }
            }
            crate::http::QueryParamMatch::Exact { name, value } => {
                HTTPRouteRulesMatchesQueryParams {
                    name: name.clone(),
                    r#type: Some(HTTPRouteRulesMatchesQueryParamsType::Exact),
                    value: value.clone(),
                }
            }
        })
    }
}

impl TryFrom<&crate::http::PathMatch> for HTTPRouteRulesMatchesPath {
    type Error = Error;

    fn try_from(path_match: &crate::http::PathMatch) -> Result<HTTPRouteRulesMatchesPath, Error> {
        let (match_type, value) = match path_match {
            crate::http::PathMatch::Prefix { value } => {
                (HTTPRouteRulesMatchesPathType::PathPrefix, value.clone())
            }
            crate::http::PathMatch::RegularExpression { value } => (
                HTTPRouteRulesMatchesPathType::RegularExpression,
                value.to_string(),
            ),
            crate::http::PathMatch::Exact { value } => {
                (HTTPRouteRulesMatchesPathType::Exact, value.clone())
            }
        };

        Ok(HTTPRouteRulesMatchesPath {
            r#type: Some(match_type),
            value: Some(value),
        })
    }
}

impl TryFrom<&crate::http::HeaderMatch> for HTTPRouteRulesMatchesHeaders {
    type Error = Error;

    fn try_from(
        header_match: &crate::http::HeaderMatch,
    ) -> Result<HTTPRouteRulesMatchesHeaders, Error> {
        Ok(match header_match {
            crate::http::HeaderMatch::RegularExpression { name, value } => {
                HTTPRouteRulesMatchesHeaders {
                    name: name.clone(),
                    r#type: Some(HTTPRouteRulesMatchesHeadersType::RegularExpression),
                    value: value.to_string(),
                }
            }
            crate::http::HeaderMatch::Exact { name, value } => HTTPRouteRulesMatchesHeaders {
                name: name.clone(),
                r#type: Some(HTTPRouteRulesMatchesHeadersType::Exact),
                value: value.clone(),
            },
        })
    }
}

macro_rules! into_kube_ref {
    ($target_type:ident, $target:expr, { $($extra_field:ident: $extra_value:expr)*$(,)? }) => {
        match $target {
            crate::Target::DNS(target) => $target_type {
                group: Some("junctionlabs.io".to_string()),
                kind: Some("DNS".to_string()),
                name: target.hostname.to_string(),
                port: target.port.map(|p| p as i32),
                $(
                    $extra_field: $extra_value,
                )*
                ..Default::default()
            },
            crate::Target::Service(target) => $target_type {
                group: Some(String::new()),
                kind: Some("Service".to_string()),
                name: target.name.to_string(),
                namespace: Some(target.namespace.to_string()),
                port: target.port.map(|p| p as i32),
                $(
                    $extra_field: $extra_value,
                )*
                ..Default::default()
            },
        }
    };
}

impl TryFrom<&crate::Target> for HTTPRouteParentRefs {
    type Error = Error;

    fn try_from(target: &crate::Target) -> Result<HTTPRouteParentRefs, Error> {
        Ok(into_kube_ref!(HTTPRouteParentRefs, target, {}))
    }
}

impl TryFrom<&crate::http::WeightedTarget> for HTTPRouteRulesBackendRefs {
    type Error = Error;

    fn try_from(target: &crate::http::WeightedTarget) -> Result<HTTPRouteRulesBackendRefs, Error> {
        let mut backend_ref = into_kube_ref!(HTTPRouteRulesBackendRefs, &target.target, {
            weight: Some(target.weight as i32)
        });
        // backend refs must have a port set on a Service target. force a value
        // for the port, using the default http port if there's nothing set.
        backend_ref.port.get_or_insert(80);
        Ok(backend_ref)
    }
}

#[cfg(test)]
mod test {
    use gateway_api::apis::experimental::httproutes::HTTPRoute;

    use super::*;

    #[test]
    fn test_route_from_yml() {
        let gateway_yaml = r#"
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: foo-route
spec:
  parentRefs:
  - name: example-gateway
    namespace: prod
    group: ""
    kind: Service
  rules:
  - matches:
    - path:
        type: PathPrefix
        value: /login
    retry:
      attempts: 3
      backoff: 1m2s3ms
    timeouts:
      request: 1m2s3ms
    backendRefs:
    - name: foo-svc
      namespace: prod
      port: 8080
        "#;

        let gateway_route: HTTPRoute = serde_yml::from_str(gateway_yaml).unwrap();
        let route = crate::http::Route {
            target: crate::Target::Service(crate::ServiceTarget {
                name: Name::from_static("example-gateway"),
                namespace: Name::from_static("prod"),
                port: None,
            }),
            rules: vec![crate::http::RouteRule {
                matches: vec![crate::http::RouteMatch {
                    path: Some(crate::http::PathMatch::Prefix {
                        value: "/login".to_string(),
                    }),
                    ..Default::default()
                }],
                retry: Some(crate::http::RouteRetry {
                    codes: vec![],
                    attempts: Some(3),
                    backoff: Some(Duration::from_str("1m2s3ms").unwrap()),
                }),
                timeouts: Some(crate::http::RouteTimeouts {
                    request: Some(Duration::from_str("1m2s3ms").unwrap()),
                    backend_request: None,
                }),
                backends: vec![crate::http::WeightedTarget {
                    weight: 1,
                    target: crate::Target::Service(crate::ServiceTarget {
                        name: Name::from_static("foo-svc"),
                        namespace: Name::from_static("prod"),
                        port: Some(8080),
                    }),
                }],
                ..Default::default()
            }],
        };

        assert_eq!(
            crate::http::Route::try_from(&gateway_route.spec).unwrap(),
            route,
            "should parse from gateway",
        );
        assert_eq!(
            crate::http::Route::try_from(&HTTPRouteSpec::try_from(&route).unwrap()).unwrap(),
            route,
            "should roundtrip"
        );

        // NOTE: gateway structs still don't impl PartialEq. once a patch patch comes out, try
        // to roundtrip that way.
        // NOTE: yaml doesn't roundtrip because we fill in defaults.
    }

    #[test]
    fn test_roundtrip() {
        let route = crate::http::Route {
            target: crate::Target::Service(crate::ServiceTarget {
                name: Name::from_static("example-gateway"),
                namespace: Name::from_static("default"),
                port: None,
            }),
            rules: vec![crate::http::RouteRule {
                matches: vec![crate::http::RouteMatch {
                    path: Some(crate::http::PathMatch::Prefix {
                        value: "/login".to_string(),
                    }),
                    ..Default::default()
                }],
                retry: Some(crate::http::RouteRetry {
                    codes: vec![500, 503],
                    attempts: Some(3),
                    backoff: Some(Duration::from_secs(2).unwrap()),
                }),
                timeouts: Some(crate::http::RouteTimeouts {
                    request: Some(Duration::from_secs(2).unwrap()),
                    backend_request: Some(Duration::from_secs(1).unwrap()),
                }),
                backends: vec![crate::http::WeightedTarget {
                    weight: 1,
                    target: crate::Target::Service(crate::ServiceTarget {
                        name: Name::from_static("foo-svc"),
                        namespace: Name::from_static("default"),
                        port: Some(8080),
                    }),
                }],
                ..Default::default()
            }],
        };

        assert_eq!(
            route,
            crate::http::Route::try_from(&HTTPRouteSpec::try_from(&route).unwrap()).unwrap(),
        );
    }
}
