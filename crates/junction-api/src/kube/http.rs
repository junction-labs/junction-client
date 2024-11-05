use std::collections::BTreeMap;
use std::str::FromStr;

use gateway_api::apis::experimental::httproutes::{
    HTTPRoute, HTTPRouteParentRefs, HTTPRouteRules, HTTPRouteRulesBackendRefs,
    HTTPRouteRulesMatches, HTTPRouteRulesMatchesHeaders, HTTPRouteRulesMatchesHeadersType,
    HTTPRouteRulesMatchesMethod, HTTPRouteRulesMatchesPath, HTTPRouteRulesMatchesPathType,
    HTTPRouteRulesMatchesQueryParams, HTTPRouteRulesMatchesQueryParamsType, HTTPRouteRulesRetry,
    HTTPRouteRulesTimeouts, HTTPRouteSpec,
};
use kube::api::ObjectMeta;
use kube::ResourceExt;

use crate::error::{Error, ErrorContext};
use crate::shared::Regex;
use crate::{Duration, Name, Target};

// TODO: this originally was written as a bunch of TryFrom implementations, but
// that makes organization in here very tough and go-to-definition hard. try
// re-writing those impls as to_kube(..) and from_kube(..) methods.

impl crate::http::Route {
    /// Convert an [HTTPRouteSpec] into a [Route][crate::http::Route].
    #[inline]
    pub fn from_gateway_httproute(httproute: &HTTPRoute) -> Result<crate::http::Route, Error> {
        httproute.try_into()
    }

    /// Convert this [Route][crate::http::Route] into a Gateway API [HTTPRoute]
    /// with it's `name` and `namespace` metadata set and
    /// [tags][crate::http::Route::tags] converted to annotations.
    pub fn to_gateway_httproute(&self, namespace: &str, name: &str) -> Result<HTTPRoute, Error> {
        let spec = self.try_into()?;
        let mut route = HTTPRoute {
            metadata: ObjectMeta {
                namespace: Some(namespace.to_string()),
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec,
            status: None,
        };
        write_tags(route.annotations_mut(), &self.tags);

        Ok(route)
    }
}

fn write_tags(annotations: &mut BTreeMap<String, String>, tags: &BTreeMap<String, String>) {
    for (k, v) in tags {
        let k = format!("junctionlabs.io.route.tags/{k}");
        annotations.insert(k, v.to_string());
    }
}

fn read_tags(annotations: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();

    for (k, v) in annotations {
        if let Some(key) = k.strip_prefix("junctionlabs.io.route.tags/") {
            tags.insert(key.to_string(), v.to_string());
        }
    }

    tags
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

impl TryFrom<&HTTPRoute> for crate::http::Route {
    type Error = Error;

    fn try_from(route: &HTTPRoute) -> Result<Self, Error> {
        use crate::VirtualHost;

        // build a target from the parent ref. forbid having more than one parent ref.
        //
        // TOOD: we could allow converting one HTTPRoute into more than one Route
        let vhost = match route.spec.parent_refs.as_deref() {
            Some([parent_ref]) => VirtualHost::try_from(parent_ref).with_index(0),
            Some(_) => Err(Error::new_static(
                "HTTPRoute can't have more than one parent ref",
            )),
            None => Err(Error::new_static("HTTPRoute must have a parent ref")),
        };

        let vhost = vhost.with_fields("spec", "parentRefs")?;
        let tags = read_tags(route.annotations());
        let rules = vec_from_gateway!(route.spec.rules).with_fields("spec", "rules")?;

        Ok(Self { vhost, tags, rules })
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

        let backoff = parse_duration(&retry.backoff)?;

        Ok(crate::http::RouteRetry {
            codes,
            attempts,
            backoff,
        })
    }
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

impl TryFrom<&HTTPRouteParentRefs> for crate::VirtualHost {
    type Error = Error;
    fn try_from(parent_ref: &HTTPRouteParentRefs) -> Result<Self, Error> {
        let group = parent_ref
            .group
            .as_deref()
            .unwrap_or("gateway.networking.k8s.io");

        match (group, parent_ref.kind.as_deref()) {
            ("junctionlabs.io", Some("DNS")) => {
                let target = Target::dns(&parent_ref.name).with_field("name")?;
                let port = port_from_gateway(&parent_ref.port).with_field("port")?;
                Ok(crate::VirtualHost { target, port })
            }
            ("", Some("Service")) => {
                // NOTE: kube doesn't require the namespace, but we do for now.
                let namespace = parent_ref
                    .namespace
                    .as_deref()
                    .ok_or_else(|| Error::new_static("missing namespace"))
                    .and_then(Name::from_str)
                    .with_field("namespace")?;
                let port = port_from_gateway(&parent_ref.port).with_field("port")?;

                let target =
                    Target::kube_service(&namespace, &parent_ref.name).with_field("name")?;

                Ok(crate::VirtualHost { target, port })
            }
            (group, Some(kind)) => Err(Error::new(format!(
                "unsupported parent ref: {group}/{kind}"
            ))),
            _ => Err(Error::new("missing Kind".to_string())),
        }
    }
}

impl TryFrom<&HTTPRouteRulesBackendRefs> for crate::http::WeightedBackend {
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

        let port = port_from_gateway(&backend_ref.port)
            .and_then(|p| p.ok_or(Error::new_static("backendRef port is required")))
            .with_field("port")?;

        let target = match (group, kind) {
            ("junctionlabs.io", "DNS") => Target::dns(&backend_ref.name).with_field("name")?,
            ("", "Service") => {
                // NOTE: kube doesn't require the namespace, but we do for now.
                let namespace = backend_ref
                    .namespace
                    .as_deref()
                    .ok_or_else(|| Error::new_static("missing namespace"))
                    .and_then(Name::from_str)
                    .with_field("namespace")?;

                Target::kube_service(&namespace, &backend_ref.name)?
            }
            (group, kind) => {
                return Err(Error::new(format!(
                    "unsupported backend ref: {group}/{kind}"
                )))
            }
        };

        Ok(crate::http::WeightedBackend {
            weight,
            backend: crate::BackendId { target, port },
        })
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
        let parent_ref = (&route.vhost).try_into().with_field("target")?;

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
        let request = timeouts.request.map(serialize_duration).transpose()?;
        let backend_request = timeouts
            .backend_request
            .map(serialize_duration)
            .transpose()?;

        Ok(HTTPRouteRulesTimeouts {
            backend_request,
            request,
        })
    }
}

impl TryFrom<&crate::http::RouteRetry> for HTTPRouteRulesRetry {
    type Error = Error;

    fn try_from(retry: &crate::http::RouteRetry) -> Result<Self, Self::Error> {
        let attempts = retry.attempts.map(|n| n as i64);
        let backoff = retry.backoff.map(serialize_duration).transpose()?;
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

impl TryFrom<&crate::VirtualHost> for HTTPRouteParentRefs {
    type Error = Error;

    fn try_from(target: &crate::VirtualHost) -> Result<HTTPRouteParentRefs, Error> {
        let (group, kind) = group_kind(&target.target);
        let (name, namespace) = name_and_namespace(&target.target);
        let port = target.port.map(|p| p as i32);

        Ok(HTTPRouteParentRefs {
            group: Some(group.to_string()),
            kind: Some(kind.to_string()),
            name,
            namespace,
            port,
            ..Default::default()
        })
    }
}

impl TryFrom<&crate::http::WeightedBackend> for HTTPRouteRulesBackendRefs {
    type Error = Error;

    fn try_from(
        weighted_target: &crate::http::WeightedBackend,
    ) -> Result<HTTPRouteRulesBackendRefs, Error> {
        let (group, kind) = group_kind(&weighted_target.backend.target);
        let (name, namespace) = name_and_namespace(&weighted_target.backend.target);
        let weight = Some(
            weighted_target
                .weight
                .try_into()
                .map_err(|_| Error::new_static("weight does not fit into an i32"))?,
        );

        Ok(HTTPRouteRulesBackendRefs {
            name,
            namespace,
            group: Some(group.to_string()),
            kind: Some(kind.to_string()),
            port: Some(weighted_target.backend.port as i32),
            weight,
            ..Default::default()
        })
    }
}

fn name_and_namespace(target: &Target) -> (String, Option<String>) {
    match target {
        Target::Dns(dns) => (dns.hostname.to_string(), None),
        Target::KubeService(svc) => (svc.name.to_string(), Some(svc.namespace.to_string())),
    }
}

fn group_kind(target: &Target) -> (&'static str, &'static str) {
    match target {
        Target::Dns(_) => ("junctionlabs.io", "DNS"),
        Target::KubeService(_) => ("", "Service"),
    }
}

fn parse_duration(d: &Option<String>) -> Result<Option<crate::shared::Duration>, Error> {
    use gateway_api::duration::Duration as GatewayDuration;

    let Some(d) = d else {
        return Ok(None);
    };

    let kube_duration =
        GatewayDuration::from_str(d).map_err(|e| Error::new(format!("invalid duration: {e}")))?;

    let secs = kube_duration.as_secs();
    let nanos = kube_duration.subsec_nanos();
    Ok(Some(Duration::new(secs, nanos)))
}

fn serialize_duration(d: Duration) -> Result<String, Error> {
    use gateway_api::duration::Duration as GatewayDuration;

    let kube_duration = GatewayDuration::try_from(std::time::Duration::from(d)).map_err(|e| {
        Error::new(format!(
            "failed to convert a duration to a Gateway duration: {e}"
        ))
    })?;

    Ok(kube_duration.to_string())
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;

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
      request: 4m5s6ms
    backendRefs:
    - name: foo-svc
      namespace: prod
      port: 8080
        "#;

        let gateway_route: HTTPRoute = serde_yml::from_str(gateway_yaml).unwrap();
        let route = crate::http::Route {
            vhost: Target::kube_service("prod", "example-gateway")
                .unwrap()
                .into_vhost(None),
            tags: Default::default(),
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
                    backoff: Some(Duration::from_secs_f64(62.003)),
                }),
                timeouts: Some(crate::http::RouteTimeouts {
                    request: Some(Duration::from_secs_f64(245.006)),
                    backend_request: None,
                }),
                backends: vec![crate::http::WeightedBackend {
                    weight: 1,
                    backend: Target::kube_service("prod", "foo-svc")
                        .unwrap()
                        .into_backend(8080),
                }],
                ..Default::default()
            }],
        };

        assert_eq!(
            crate::http::Route::try_from(&gateway_route).unwrap(),
            route,
            "should parse from gateway",
        );
        assert_eq!(
            crate::http::Route::try_from(&route.to_gateway_httproute("potato", "tomato").unwrap())
                .unwrap(),
            route,
            "should roundtrip"
        );
    }

    #[test]
    fn test_passsthrough_route() {
        let kube_route = HTTPRoute {
            metadata: ObjectMeta {
                namespace: Some("prod".to_string()),
                name: Some("web".to_string()),
                ..Default::default()
            },
            spec: HTTPRouteSpec {
                parent_refs: Some(vec![HTTPRouteParentRefs {
                    group: Some("".to_string()),
                    kind: Some("Service".to_string()),
                    namespace: Some("prod".to_string()),
                    name: "cool-service".to_string(),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            status: None,
        };

        assert_eq!(
            crate::http::Route::from_gateway_httproute(&kube_route).unwrap(),
            crate::http::Route {
                vhost: Target::kube_service("prod", "cool-service")
                    .unwrap()
                    .into_vhost(None),
                tags: Default::default(),
                rules: vec![],
            }
        );
    }

    #[test]
    fn test_roundtrip() {
        let route = crate::http::Route {
            vhost: Target::kube_service("default", "example-gateway")
                .unwrap()
                .into_vhost(None),
            tags: BTreeMap::from_iter([
                ("foo".to_string(), "bar".to_string()),
                ("one".to_string(), "seven".to_string()),
            ]),
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
                    backoff: Some(Duration::from_secs(2)),
                }),
                timeouts: Some(crate::http::RouteTimeouts {
                    request: Some(Duration::from_secs(2)),
                    backend_request: Some(Duration::from_secs(1)),
                }),
                backends: vec![crate::http::WeightedBackend {
                    weight: 1,
                    backend: Target::kube_service("default", "foo-svc")
                        .unwrap()
                        .into_backend(8080),
                }],
                ..Default::default()
            }],
        };

        assert_eq!(
            route,
            crate::http::Route::from_gateway_httproute(
                &route.to_gateway_httproute("potato", "tomato").unwrap(),
            )
            .unwrap(),
        );
    }
}
