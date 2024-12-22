use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;

use gateway_api::apis::experimental::httproutes as gateway_http;
use kube::api::ObjectMeta;
use kube::{Resource, ResourceExt};

use crate::error::{Error, ErrorContext};
use crate::http::{
    BackendRef, HeaderMatch, HostnameMatch, PathMatch, QueryParamMatch, Route, RouteMatch,
    RouteRetry, RouteRule, RouteTimeouts,
};
use crate::shared::Regex;
use crate::{Duration, Name, Service};

macro_rules! option_from_gateway {
    ($t:ty, $field:expr, $field_name:literal) => {
        $field
            .as_ref()
            .map(|v| <$t>::from_gateway(v).with_field($field_name))
            .transpose()
    };
}

macro_rules! vec_from_gateway {
    ($t:ty, $opt_vec:expr, $field_name:literal) => {
        $opt_vec
            .iter()
            .flatten()
            .enumerate()
            .map(|(i, e)| <$t>::from_gateway(e).with_field_index($field_name, i))
            .collect::<Result<Vec<_>, _>>()
    };
}

macro_rules! vec_from_gateway_p {
    ($param:expr, $t:ty, $opt_vec:expr, $field_name:literal) => {
        $opt_vec
            .iter()
            .flatten()
            .enumerate()
            .map(|(i, e)| <$t>::from_gateway($param, e).with_field_index($field_name, i))
            .collect::<Result<Vec<_>, _>>()
    };
}

macro_rules! option_to_gateway {
    ($field:expr) => {
        $field.as_ref().map(|e| e.to_gateway())
    };
}

macro_rules! vec_to_gateway {
    ($vec:expr) => {
        $vec.iter().map(|e| e.to_gateway()).collect()
    };
    ($vec:expr, err_field = $field_name:literal) => {
        $vec.iter()
            .enumerate()
            .map(|(i, e)| e.to_gateway().with_field_index($field_name, i))
            .collect::<Result<Vec<_>, Error>>()
    };
}

impl Route {
    /// Convert a Gateway API [HTTPRoute][gateway_http::HTTPRoute] into a Junction [Route].
    pub fn from_gateway_httproute(httproute: &gateway_http::HTTPRoute) -> Result<Route, Error> {
        let id = Name::from_str(httproute.meta().name.as_ref().unwrap()).unwrap();
        let namespace = httproute.meta().namespace.as_deref().unwrap_or("default");
        let (hostnames, ports) = from_parent_refs(
            namespace,
            httproute.spec.parent_refs.as_deref().unwrap_or_default(),
        )?;
        let tags = read_tags(httproute.annotations());
        let rules = vec_from_gateway_p!(namespace, RouteRule, httproute.spec.rules, "rules")
            .with_field("spec")?;

        Ok(Self {
            id,
            hostnames,
            ports,
            tags,
            rules,
        })
    }

    /// Convert this Route to a Gateway API [HTTPRoute][gateway_http::HTTPRoute].
    pub fn to_gateway_httproute(&self, namespace: &str) -> Result<gateway_http::HTTPRoute, Error> {
        let parent_refs = Some(to_parent_refs(&self.hostnames, &self.ports)?);

        let spec = gateway_http::HTTPRouteSpec {
            hostnames: None,
            parent_refs,
            rules: Some(vec_to_gateway!(self.rules, err_field = "rules")?),
        };

        let mut route = gateway_http::HTTPRoute {
            metadata: ObjectMeta {
                namespace: Some(namespace.to_string()),
                name: Some(self.id.to_string()),
                ..Default::default()
            },
            spec,
            status: None,
        };
        write_tags(route.annotations_mut(), &self.tags);

        Ok(route)
    }
}

fn from_parent_refs(
    route_namespace: &str,
    parent_refs: &[gateway_http::HTTPRouteParentRefs],
) -> Result<(Vec<HostnameMatch>, Vec<u16>), Error> {
    let mut hostnames_and_ports: BTreeMap<_, HashSet<u16>> = BTreeMap::new();

    for parent_ref in parent_refs {
        let group = parent_ref
            .group
            .as_deref()
            .unwrap_or("gateway.networking.k8s.io");
        let kind = parent_ref.kind.as_deref().unwrap_or("");

        let hostname = match (group, kind) {
            ("junctionlabs.io", "DNS") => HostnameMatch::from_str(&parent_ref.name)?,
            ("", "Service") => {
                let namespace = parent_ref.namespace.as_deref().unwrap_or(route_namespace);

                // generate the kube service Hostname and convert it to a match
                crate::Service::kube(namespace, &parent_ref.name)?
                    .hostname()
                    .into()
            }
            (group, kind) => {
                return Err(Error::new(format!(
                    "unsupported backend ref: {group}/{kind}"
                )))
            }
        };

        let port_list = hostnames_and_ports.entry(hostname).or_default();
        if let &Some(port) = &parent_ref.port {
            let port: u16 = port
                .try_into()
                .map_err(|_| Error::new_static("invalid u16"))?;
            port_list.insert(port);
        }
    }

    if !all_same(hostnames_and_ports.values()) {
        return Err(Error::new_static(
            "parentRefs do not all have the same list of ports",
        ));
    }

    // grab the first port set and turn it into a vec. we've already validated
    // that all the port sets are the same, so only the first one matters.
    let ports = hostnames_and_ports
        .values()
        .next()
        .cloned()
        .unwrap_or_default();
    let mut ports: Vec<_> = ports.into_iter().collect();
    ports.sort();

    // grab the hostnames and peel out.
    let mut hostnames: Vec<_> = hostnames_and_ports.into_keys().collect();
    hostnames.sort();

    Ok((hostnames, ports))
}

fn all_same<T: Eq>(iter: impl Iterator<Item = T>) -> bool {
    let mut prev = None;
    for item in iter {
        match prev.take() {
            Some(prev) if prev != item => return false,
            _ => (),
        }

        prev = Some(item)
    }

    true
}

fn to_parent_refs(
    hostnames: &[HostnameMatch],
    ports: &[u16],
) -> Result<Vec<gateway_http::HTTPRouteParentRefs>, Error> {
    let mut parent_refs = Vec::with_capacity(hostnames.len() * ports.len());

    for hostname in hostnames {
        let parent_ref = hostname_match_to_parentref(hostname)?;

        for &port in ports {
            let mut parent_ref = parent_ref.clone();
            parent_ref.port = Some(port as i32);
            parent_refs.push(parent_ref);
        }

        if ports.is_empty() {
            parent_refs.push(parent_ref);
        }
    }

    Ok(parent_refs)
}

fn hostname_match_to_parentref(
    hostname_match: &HostnameMatch,
) -> Result<gateway_http::HTTPRouteParentRefs, Error> {
    let (svc, name, namespace) = match hostname_match {
        // subdomains are always treated as DNS parentRefs. this doesn't use
        // name_and_namespace so that the wildcard makes it into the name.
        HostnameMatch::Subdomain(hostname) => {
            let svc = Service::Dns(crate::DnsService {
                hostname: hostname.clone(),
            });
            let name = hostname_match.to_string();
            (svc, name, None)
        }
        HostnameMatch::Exact(hostname) => {
            let svc = Service::from_str(hostname)?;
            let (name, namespace) = name_and_namespace(&svc);
            (svc, name, namespace)
        }
    };

    let (group, kind) = group_kind(&svc);
    Ok(gateway_http::HTTPRouteParentRefs {
        group: Some(group.to_string()),
        kind: Some(kind.to_string()),
        name,
        namespace,
        ..Default::default()
    })
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

fn port_from_gateway(port: &Option<i32>) -> Result<Option<u16>, Error> {
    (*port)
        .map(|p| {
            p.try_into()
                .map_err(|_| Error::new_static("port value out of range"))
        })
        .transpose()
}

macro_rules! method_matches {
    ( $($method:ident => $str:expr,)* $(,)*) => {
        fn method_from_gateway(match_method: &gateway_http::HTTPRouteRulesMatchesMethod) -> Result<crate::http::Method, Error> {
            match match_method {
                $(
                    gateway_http::HTTPRouteRulesMatchesMethod::$method => Ok($str.to_string()),
                )*
            }
        }

        fn method_to_gateway(method: &crate::http::Method) -> Result<gateway_http::HTTPRouteRulesMatchesMethod, Error> {
            match method.as_str() {
                $(
                    $str => Ok(gateway_http::HTTPRouteRulesMatchesMethod::$method),
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

impl RouteRule {
    fn to_gateway(&self) -> Result<gateway_http::HTTPRouteRules, Error> {
        Ok(gateway_http::HTTPRouteRules {
            name: self.name.as_ref().map(|s| s.to_string()),
            backend_refs: Some(vec_to_gateway!(self.backends, err_field = "backends")?),
            filters: None,
            matches: Some(vec_to_gateway!(self.matches, err_field = "matches")?),
            retry: option_to_gateway!(self.retry)
                .transpose()
                .with_field("retry")?,
            session_persistence: None,
            timeouts: option_to_gateway!(self.timeouts)
                .transpose()
                .with_field("timeouts")?,
        })
    }

    fn from_gateway(
        route_namespace: &str,
        rule: &gateway_http::HTTPRouteRules,
    ) -> Result<Self, Error> {
        let name = rule
            .name
            .as_ref()
            .map(|s| Name::from_str(s))
            .transpose()
            .with_field("name")?;

        let matches = vec_from_gateway!(RouteMatch, rule.matches, "matches")?;
        let timeouts = option_from_gateway!(RouteTimeouts, rule.timeouts, "timeouts")?;
        let backends =
            vec_from_gateway_p!(route_namespace, BackendRef, rule.backend_refs, "backends")?;
        let retry = option_from_gateway!(RouteRetry, rule.retry, "retry")?;

        // FIXME: filters are ignored because they're not implemented yet
        let filters = vec![];

        Ok(RouteRule {
            name,
            matches,
            filters,
            timeouts,
            retry,
            backends,
        })
    }
}

impl RouteTimeouts {
    fn to_gateway(&self) -> Result<gateway_http::HTTPRouteRulesTimeouts, Error> {
        let request = self.request.map(serialize_duration).transpose()?;
        let backend_request = self.backend_request.map(serialize_duration).transpose()?;
        Ok(gateway_http::HTTPRouteRulesTimeouts {
            backend_request,
            request,
        })
    }

    fn from_gateway(timeouts: &gateway_http::HTTPRouteRulesTimeouts) -> Result<Self, Error> {
        let request = parse_duration(&timeouts.request).with_field("request")?;
        let backend_request =
            parse_duration(&timeouts.backend_request).with_field("backendRequest")?;

        Ok(RouteTimeouts {
            request,
            backend_request,
        })
    }
}

impl RouteRetry {
    fn to_gateway(&self) -> Result<gateway_http::HTTPRouteRulesRetry, Error> {
        let attempts = self.attempts.map(|n| n as i64);
        let backoff = self.backoff.map(serialize_duration).transpose()?;
        let codes = if self.codes.is_empty() {
            None
        } else {
            let codes = self.codes.iter().map(|&code| code as i64).collect();
            Some(codes)
        };

        Ok(gateway_http::HTTPRouteRulesRetry {
            attempts,
            backoff,
            codes,
        })
    }

    fn from_gateway(retry: &gateway_http::HTTPRouteRulesRetry) -> Result<Self, Error> {
        let mut codes = Vec::with_capacity(retry.codes.as_ref().map_or(0, |c| c.len()));
        for (i, &code) in retry.codes.iter().flatten().enumerate() {
            let code: u16 = code
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

        Ok(RouteRetry {
            codes,
            attempts,
            backoff,
        })
    }
}

impl RouteMatch {
    fn to_gateway(&self) -> Result<gateway_http::HTTPRouteRulesMatches, Error> {
        let method = self
            .method
            .as_ref()
            .map(method_to_gateway)
            .transpose()
            .with_field("method")?;

        Ok(gateway_http::HTTPRouteRulesMatches {
            headers: Some(vec_to_gateway!(&self.headers)),
            method,
            path: option_to_gateway!(&self.path),
            query_params: Some(vec_to_gateway!(&self.query_params)),
        })
    }

    fn from_gateway(matches: &gateway_http::HTTPRouteRulesMatches) -> Result<Self, Error> {
        let method = matches
            .method
            .as_ref()
            .map(method_from_gateway)
            .transpose()
            .with_field("method")?;

        Ok(RouteMatch {
            path: option_from_gateway!(PathMatch, matches.path, "path")?,
            headers: vec_from_gateway!(HeaderMatch, matches.headers, "headers")?,
            query_params: vec_from_gateway!(QueryParamMatch, matches.query_params, "queryParams")?,
            method,
        })
    }
}

impl QueryParamMatch {
    fn to_gateway(&self) -> gateway_http::HTTPRouteRulesMatchesQueryParams {
        match self {
            QueryParamMatch::RegularExpression { name, value } => {
                gateway_http::HTTPRouteRulesMatchesQueryParams {
                    name: name.clone(),
                    r#type: Some(
                        gateway_http::HTTPRouteRulesMatchesQueryParamsType::RegularExpression,
                    ),
                    value: value.to_string(),
                }
            }
            QueryParamMatch::Exact { name, value } => {
                gateway_http::HTTPRouteRulesMatchesQueryParams {
                    name: name.clone(),
                    r#type: Some(gateway_http::HTTPRouteRulesMatchesQueryParamsType::Exact),
                    value: value.clone(),
                }
            }
        }
    }

    fn from_gateway(
        matches_query: &gateway_http::HTTPRouteRulesMatchesQueryParams,
    ) -> Result<Self, Error> {
        let name = &matches_query.name;
        let value = &matches_query.value;
        match matches_query.r#type {
            Some(gateway_http::HTTPRouteRulesMatchesQueryParamsType::Exact) => {
                Ok(QueryParamMatch::Exact {
                    name: name.clone(),
                    value: value.clone(),
                })
            }
            Some(gateway_http::HTTPRouteRulesMatchesQueryParamsType::RegularExpression) => {
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

impl PathMatch {
    fn to_gateway(&self) -> gateway_http::HTTPRouteRulesMatchesPath {
        let (match_type, value) = match self {
            PathMatch::Prefix { value } => (
                gateway_http::HTTPRouteRulesMatchesPathType::PathPrefix,
                value.clone(),
            ),
            PathMatch::RegularExpression { value } => (
                gateway_http::HTTPRouteRulesMatchesPathType::RegularExpression,
                value.to_string(),
            ),
            PathMatch::Exact { value } => (
                gateway_http::HTTPRouteRulesMatchesPathType::Exact,
                value.clone(),
            ),
        };

        gateway_http::HTTPRouteRulesMatchesPath {
            r#type: Some(match_type),
            value: Some(value),
        }
    }

    fn from_gateway(matches_path: &gateway_http::HTTPRouteRulesMatchesPath) -> Result<Self, Error> {
        let Some(value) = &matches_path.value else {
            return Err(Error::new_static("missing value"));
        };

        match matches_path.r#type {
            Some(gateway_http::HTTPRouteRulesMatchesPathType::Exact) => Ok(PathMatch::Exact {
                value: value.clone(),
            }),
            Some(gateway_http::HTTPRouteRulesMatchesPathType::PathPrefix) => {
                Ok(PathMatch::Prefix {
                    value: value.clone(),
                })
            }
            Some(gateway_http::HTTPRouteRulesMatchesPathType::RegularExpression) => {
                let value = Regex::from_str(value)
                    .map_err(|e| Error::new(format!("invalid regex: {e}")).with_field("value"))?;
                Ok(PathMatch::RegularExpression { value })
            }
            None => Err(Error::new_static("missing type")),
        }
    }
}

impl HeaderMatch {
    fn to_gateway(&self) -> gateway_http::HTTPRouteRulesMatchesHeaders {
        match self {
            HeaderMatch::RegularExpression { name, value } => {
                gateway_http::HTTPRouteRulesMatchesHeaders {
                    name: name.clone(),
                    r#type: Some(gateway_http::HTTPRouteRulesMatchesHeadersType::RegularExpression),
                    value: value.to_string(),
                }
            }
            HeaderMatch::Exact { name, value } => gateway_http::HTTPRouteRulesMatchesHeaders {
                name: name.clone(),
                r#type: Some(gateway_http::HTTPRouteRulesMatchesHeadersType::Exact),
                value: value.clone(),
            },
        }
    }

    fn from_gateway(
        matches_headers: &gateway_http::HTTPRouteRulesMatchesHeaders,
    ) -> Result<Self, Error> {
        let name = &matches_headers.name;
        let value = &matches_headers.value;
        match matches_headers.r#type {
            Some(gateway_http::HTTPRouteRulesMatchesHeadersType::Exact) => Ok(HeaderMatch::Exact {
                name: name.clone(),
                value: value.clone(),
            }),
            Some(gateway_http::HTTPRouteRulesMatchesHeadersType::RegularExpression) => {
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

impl BackendRef {
    fn from_gateway(
        route_namespace: &str,
        backend_ref: &gateway_http::HTTPRouteRulesBackendRefs,
    ) -> Result<Self, Error> {
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

        let service = match (group, kind) {
            ("junctionlabs.io", "DNS") => {
                crate::Service::dns(&backend_ref.name).with_field("name")?
            }
            ("", "Service") => {
                let namespace = backend_ref.namespace.as_deref().unwrap_or(route_namespace);
                crate::Service::kube(namespace, &backend_ref.name)?
            }
            (group, kind) => {
                return Err(Error::new(format!(
                    "unsupported backend ref: {group}/{kind}"
                )))
            }
        };

        Ok(BackendRef {
            service,
            port: Some(port),
            weight,
        })
    }

    fn to_gateway(&self) -> Result<gateway_http::HTTPRouteRulesBackendRefs, Error> {
        let (group, kind) = group_kind(&self.service);
        let (name, namespace) = name_and_namespace(&self.service);
        let weight = Some(
            self.weight
                .try_into()
                .map_err(|_| Error::new_static("weight cannot be converted to an i32"))
                .with_field("weight")?,
        );
        if self.port.is_none() {
            return Err(Error::new_static(
                "backendRefs must have a port set when converting to an HTTPRoute",
            )
            .with_field("port"));
        }

        Ok(gateway_http::HTTPRouteRulesBackendRefs {
            name,
            namespace,
            group: Some(group.to_string()),
            kind: Some(kind.to_string()),
            port: self.port.map(|p| p as i32),
            weight,
            ..Default::default()
        })
    }
}

fn name_and_namespace(target: &Service) -> (String, Option<String>) {
    match target {
        Service::Dns(dns) => (dns.hostname.to_string(), None),
        Service::Kube(svc) => (svc.name.to_string(), Some(svc.namespace.to_string())),
    }
}

fn group_kind(target: &Service) -> (&'static str, &'static str) {
    match target {
        Service::Dns(_) => ("junctionlabs.io", "DNS"),
        Service::Kube(_) => ("", "Service"),
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
    use serde_json::json;

    use crate::Hostname;

    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_from_gateway_simple_httproute() {
        assert_from_gateway(
            json!(
                {
                    "apiVersion": "gateway.networking.k8s.io/v1",
                    "kind": "HTTPRoute",
                    "metadata": {
                      "name": "example-route"
                    },
                    "spec": {
                      "parentRefs": [
                        {"name": "foo-svc", "namespace": "prod", "group": "", "kind": "Service"},
                      ],
                      "rules": [
                        {
                          "backendRefs": [
                            {"name": "foo-svc", "namespace": "prod", "port": 8080},
                          ]
                        }
                      ]
                    }
                  }
            ),
            Route {
                id: Name::from_static("example-route"),
                hostnames: vec![Hostname::from_static("foo-svc.prod.svc.cluster.local").into()],
                ports: vec![],
                tags: Default::default(),
                rules: vec![RouteRule {
                    backends: vec![BackendRef {
                        weight: 1,
                        service: Service::kube("prod", "foo-svc").unwrap(),
                        port: Some(8080),
                    }],
                    ..Default::default()
                }],
            },
        );
    }

    #[test]
    fn test_from_gateway_full_httproute() {
        assert_from_gateway(
            json!(
                {
                    "apiVersion": "gateway.networking.k8s.io/v1",
                    "kind": "HTTPRoute",
                    "metadata": {
                      "name": "example-route"
                    },
                    "spec": {
                      "parentRefs": [
                        {"name": "foo-svc", "namespace": "prod", "group": "", "kind": "Service", "port": 80},
                        {"name": "foo-svc", "namespace": "prod", "group": "", "kind": "Service", "port": 443},
                        {"name": "bar-svc", "namespace": "prod", "group": "", "kind": "Service", "port": 80},
                        {"name": "bar-svc", "namespace": "prod", "group": "", "kind": "Service", "port": 443},
                        {"name": "*.s3.internal", "group": "junctionlabs.io", "kind": "DNS", "port": 80},
                        {"name": "*.s3.internal", "group": "junctionlabs.io", "kind": "DNS", "port": 443 }
                      ],
                      "rules": [
                        {
                          "name": "a-name",
                          "matches": [
                            {
                              "path": {
                                "type": "PathPrefix",
                                "value": "/login"
                              }
                            }
                          ],
                          "retry": {
                            "attempts": 3,
                            "backoff": "1m2s3ms"
                          },
                          "timeouts": {
                            "request": "4m5s6ms"
                          },
                          "backendRefs": [
                            {
                              "name": "foo-svc",
                              "namespace": "prod",
                              "port": 8080
                            }
                          ]
                        },
                        {
                          "backendRefs": [
                            {
                              "name": "bar-svc",
                              "namespace": "prod",
                              "port": 8080
                            }
                          ]
                        }
                      ]
                    }
                  }
            ),
            Route {
                id: Name::from_static("example-route"),
                hostnames: [
                    "*.s3.internal",
                    "bar-svc.prod.svc.cluster.local",
                    "foo-svc.prod.svc.cluster.local",
                ]
                .into_iter()
                .map(|s| HostnameMatch::from_str(s).unwrap())
                .collect(),
                ports: vec![80, 443],
                tags: Default::default(),
                rules: vec![
                    RouteRule {
                        name: Some(Name::from_static("a-name")),
                        matches: vec![RouteMatch {
                            path: Some(PathMatch::Prefix {
                                value: "/login".to_string(),
                            }),
                            ..Default::default()
                        }],
                        retry: Some(RouteRetry {
                            codes: vec![],
                            attempts: Some(3),
                            backoff: Some(Duration::from_secs_f64(62.003)),
                        }),
                        timeouts: Some(RouteTimeouts {
                            request: Some(Duration::from_secs_f64(245.006)),
                            backend_request: None,
                        }),
                        backends: vec![BackendRef {
                            weight: 1,
                            service: Service::kube("prod", "foo-svc").unwrap(),
                            port: Some(8080),
                        }],
                        ..Default::default()
                    },
                    RouteRule {
                        backends: vec![BackendRef {
                            weight: 1,
                            service: Service::kube("prod", "bar-svc").unwrap(),
                            port: Some(8080),
                        }],
                        ..Default::default()
                    },
                ],
            },
        );
    }

    #[test]
    fn test_from_gateway_no_namespaces() {
        assert_from_gateway(
            json!(
                {
                    "apiVersion": "gateway.networking.k8s.io/v1",
                    "kind": "HTTPRoute",
                    "metadata": {
                      "name": "example-route",
                      "namespace": "prod"
                    },
                    "spec": {
                      "parentRefs": [
                        {"name": "foo-svc", "group": "", "kind": "Service", "port": 80},
                      ],
                      "rules": [
                        {
                          "backendRefs": [
                            {
                              "name": "bar-svc",
                              "port": 8080
                            }
                          ]
                        }
                      ]
                    }
                  }
            ),
            Route {
                id: Name::from_static("example-route"),
                hostnames: ["foo-svc.prod.svc.cluster.local"]
                    .into_iter()
                    .map(|s| HostnameMatch::from_str(s).unwrap())
                    .collect(),
                ports: vec![80],
                tags: Default::default(),
                rules: vec![RouteRule {
                    backends: vec![BackendRef {
                        weight: 1,
                        service: Service::kube("prod", "bar-svc").unwrap(),
                        port: Some(8080),
                    }],
                    ..Default::default()
                }],
            },
        );
    }

    #[track_caller]
    fn assert_from_gateway(gateway_spec: serde_json::Value, expected: Route) {
        let gateway_route: gateway_http::HTTPRoute = serde_json::from_value(gateway_spec).unwrap();
        assert_eq!(
            Route::from_gateway_httproute(&gateway_route).unwrap(),
            expected,
        );
    }

    #[test]
    fn test_from_gateway_invalid_parent_refs() {
        assert_from_gateway_err(json!(
            {
                "apiVersion": "gateway.networking.k8s.io/v1",
                "kind": "HTTPRoute",
                "metadata": {
                  "name": "example-route"
                },
                "spec": {
                  "parentRefs": [
                    // the two services here have different port values
                    {"name": "foo-svc", "namespace": "prod", "group": "", "kind": "Service", "port": 80},
                    {"name": "bar-svc", "namespace": "prod", "group": "", "kind": "Service", "port": 443},
                  ],
                  "rules": [
                    {
                      "backendRefs": [
                        {"name": "foo-svc", "namespace": "prod", "port": 8080},
                      ]
                    }
                  ]
                }
              }
        ));

        assert_from_gateway_err(json!(
            {
                "apiVersion": "gateway.networking.k8s.io/v1",
                "kind": "HTTPRoute",
                "metadata": {
                  "name": "example-route"
                },
                "spec": {
                  "parentRefs": [
                    // one specifies a port, the other does not. this is illegal by the gateway spec
                    {"name": "foo-svc", "namespace": "prod", "group": "", "kind": "Service"},
                    {"name": "bar-svc", "namespace": "prod", "group": "", "kind": "Service", "port": 443},
                  ],
                  "rules": [
                    {
                      "backendRefs": [
                        {"name": "foo-svc", "namespace": "prod", "port": 8080},
                      ]
                    }
                  ]
                }
              }
        ));
    }

    #[track_caller]
    fn assert_from_gateway_err(gateway_spec: serde_json::Value) {
        let gateway_route: gateway_http::HTTPRoute = serde_json::from_value(gateway_spec).unwrap();
        assert!(
            Route::from_gateway_httproute(&gateway_route).is_err(),
            "expcted an invalid route, but deserialized ok",
        )
    }

    #[test]
    fn test_roundtrip_simple_route() {
        assert_roundrip(Route {
            id: Name::from_static("simple-route"),
            hostnames: vec!["foo.bar".parse().unwrap()],
            ports: vec![],
            tags: Default::default(),
            rules: vec![RouteRule {
                backends: vec![BackendRef {
                    weight: 1,
                    service: Service::kube("default", "foo-svc").unwrap(),
                    port: Some(8080),
                }],
                ..Default::default()
            }],
        });
    }

    #[test]
    fn test_roundtrip_full_route() {
        assert_roundrip(Route {
            id: Name::from_static("full-route"),
            hostnames: vec!["*.foo.bar".parse().unwrap(), "foo.bar".parse().unwrap()],
            ports: vec![80, 8080],
            tags: BTreeMap::from_iter([
                ("foo".to_string(), "bar".to_string()),
                ("one".to_string(), "seven".to_string()),
            ]),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::Prefix {
                        value: "/login".to_string(),
                    }),
                    ..Default::default()
                }],
                retry: Some(RouteRetry {
                    codes: vec![500, 503],
                    attempts: Some(3),
                    backoff: Some(Duration::from_secs(2)),
                }),
                timeouts: Some(RouteTimeouts {
                    request: Some(Duration::from_secs(2)),
                    backend_request: Some(Duration::from_secs(1)),
                }),
                backends: vec![BackendRef {
                    weight: 1,
                    service: Service::kube("default", "foo-svc").unwrap(),
                    port: Some(8080),
                }],
                ..Default::default()
            }],
        });
    }

    #[track_caller]
    fn assert_roundrip(route: Route) {
        assert_eq!(
            route,
            Route::from_gateway_httproute(
                &route.to_gateway_httproute("a-namespace-test").unwrap(),
            )
            .unwrap(),
        );
    }
}
