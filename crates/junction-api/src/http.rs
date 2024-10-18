//! HTTP routing configuration.
//!
//! A [Route] specifies the all of the top-level configuration for all HTTP
//! traffic that matches a particular URL.

use crate::{
    shared::{Duration, Fraction, Regex},
    PortNumber, PreciseHostname, Target,
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

/// High level policy that describes how a request to a specific hostname should
/// be routed.
///
/// Routes contain a target that describes the hostname to match and at least
/// one [RouteRule]. When a [RouteRule] matches, it also describes where and how
/// the traffic should be directed to a [Backend](crate::backend::Backend).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Route {
    /// The target for this route. The target determines the hostnames that map
    /// to this route.
    pub target: Target,

    /// The route rules that determine whether any URLs match.
    pub rules: Vec<RouteRule>,
}

impl Route {
    /// Create a trivial route that passes all traffic for a target directly to
    /// the same backend target.
    pub fn passthrough_route(target: Target) -> Route {
        let backend = WeightedTarget {
            weight: 1,
            target: target.clone(),
        };
        Route {
            target,
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::empty_prefix()),
                    ..Default::default()
                }],
                backends: vec![backend],
                ..Default::default()
            }],
        }
    }

    #[doc(hidden)]
    pub fn is_passthrough_route(&self) -> bool {
        let rule = match &self.rules.as_slice() {
            &[rule] => rule,
            _ => return false,
        };

        let route_match = match &rule.matches.as_slice() {
            &[route_match] => route_match,
            _ => return false,
        };

        // one backend
        rule.backends.len() == 1
            // nothing else set
            && rule.filters.is_empty()
            && rule.timeouts.is_none()
            && rule.retry.is_none()
            // route_match is the empty path prefix match
            && route_match == &RouteMatch {
                path: Some(PathMatch::empty_prefix()),
                ..Default::default()
            }
    }
}

/// Defines semantics for matching an HTTP request based on conditions (matches), processing it
/// (filters), and forwarding the request to an API object (backendRefs).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteRule {
    /// Defines conditions used for matching the rule against incoming HTTP requests. Each match is
    /// independent, i.e. this rule will be matched if **any** one of the matches is satisfied.
    ///
    /// For example, take the following matches configuration::
    ///
    ///  ```yaml
    ///  matches:
    ///  - path:
    ///      value: "/foo"
    ///    headers:
    ///    - name: "version"
    ///      value: "v2"
    ///  - path:
    ///      value: "/v2/foo"
    ///  ```
    ///
    /// For a request to match against this rule, a request must satisfy EITHER of the two
    /// conditions:
    ///
    /// - path prefixed with `/foo` AND contains the header `version: v2`
    /// - path prefix of `/v2/foo`
    ///
    /// See the documentation for RouteMatch on how to specify multiple match conditions that should
    /// be ANDed together.
    ///
    /// If no matches are specified, the default is a prefix path match on "/", which has the effect
    /// of matching every HTTP request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<RouteMatch>,

    /// Define the filters that are applied to requests that match this rule.
    ///
    /// The effects of ordering of multiple behaviors are currently unspecified.
    ///
    /// Specifying the same filter multiple times is not supported unless explicitly indicated in
    /// the filter.
    ///
    /// All filters are compatible with each other except for the URLRewrite and RequestRedirect
    /// filters, which may not be combined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[doc(hidden)]
    pub filters: Vec<RouteFilter>,

    //FIXME(persistence): enable session persistence as per the Gateway API #[serde(default,
    //skip_serializing_if = "Option::is_none")] pub session_persistence: Option<SessionPersistence>,

    // The timeouts set on any request that matches route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<RouteTimeouts>,

    /// How to retry any requests to this route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RouteRetry>,

    /// Where the traffic should route if this rule matches.
    pub backends: Vec<WeightedTarget>,
}

/// Defines timeouts that can be configured for a HTTP Route.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteTimeouts {
    /// Specifies the maximum duration for a HTTP request. This timeout is intended to cover as
    /// close to the whole request-response transaction as possible.
    ///
    /// An entire client HTTP transaction may result in more than one call to destination backends,
    /// for example, if automatic retries are supported.
    ///
    /// Specifying a zero value such as "0s" is interpreted as no timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Duration>,

    /// Specifies a timeout for an individual request to a backend. This covers the time from when
    /// the request first starts being sent to when the full response has been received from the
    /// backend.
    ///
    /// An entire client HTTP transaction may result in more than one call to the destination
    /// backend, for example, if retries are configured.
    ///
    /// Because the Request timeout encompasses the BackendRequest timeout, the value of
    /// BackendRequest must be <= the value of Request timeout.
    ///
    /// Specifying a zero value such as "0s" is interpreted as no timeout.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "backendRequest"
    )]
    pub backend_request: Option<Duration>,
}

/// Defines the predicate used to match requests to a given action. Multiple match types are ANDed
/// together, i.e. the match will evaluate to true only if all conditions are satisfied.
///
/// For example, the match below will match a HTTP request only if its path starts with `/foo` AND
/// it contains the `version: v1` header::
///
///  ```yaml
///  match:
///    path:
///      value: "/foo"
///    headers:
///    - name: "version"
///      value "v1"
///  ```
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteMatch {
    /// Specifies a HTTP request path matcher. If this field is not specified, a default prefix
    /// match on the "/" path is provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathMatch>,

    /// Specifies HTTP request header matchers. Multiple match values are ANDed together, meaning, a
    /// request must match all the specified headers to select the route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderMatch>,

    /// Specifies HTTP query parameter matchers. Multiple match values are ANDed together, meaning,
    /// a request must match all the specified query parameters to select the route.
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "queryParams")]
    pub query_params: Vec<QueryParamMatch>,

    /// Specifies HTTP method matcher. When specified, this route will be matched only if the
    /// request has the specified method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Method>,
}

/// Describes how to select a HTTP route by matching the HTTP request path.
///
/// The `type` specifies the semantics of how HTTP paths should be compared. Valid PathMatchType
/// values are:
///
/// * "Exact"
/// * "PathPrefix"
/// * "RegularExpression"
///
/// PathPrefix and Exact paths must be syntactically valid:
/// - Must begin with the `/` character
/// - Must not contain consecutive `/` characters (e.g. `/foo///`, `//`)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum PathMatch {
    #[serde(alias = "prefix")]
    Prefix { value: String },

    #[serde(alias = "regularExpression", alias = "regular_expression")]
    RegularExpression { value: Regex },

    #[serde(untagged)]
    Exact { value: String },
}

impl PathMatch {
    /// Return a [PathMatch] that matches the empty prefix.
    ///
    /// The empty prefix matches every path, so any matcher using this will
    /// always return `true`.
    pub fn empty_prefix() -> Self {
        Self::Prefix {
            value: String::new(),
        }
    }
}

/// The name of an HTTP header.
///
/// Valid values include:
///
/// * "Authorization"
/// * "Set-Cookie"
///
/// Invalid values include:
///
/// * ":method" - ":" is an invalid character. This means that HTTP/2 pseudo headers are not
///   currently supported by this type.
/// * "/invalid" - "/" is an invalid character
pub type HeaderName = String;

/// Describes how to select a HTTP route by matching HTTP request headers.
///
/// `name` is the name of the HTTP Header to be matched. Name matching is case insensitive. (See
/// <https://tools.ietf.org/html/rfc7230#section-3.2>).
///
/// If multiple entries specify equivalent header names, only the first entry with an equivalent
/// name WILL be considered for a match. Subsequent entries with an equivalent header name WILL be
/// ignored. Due to the case-insensitivity of header names, "foo" and "Foo" are considered
/// equivalent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum HeaderMatch {
    #[serde(
        alias = "regex",
        alias = "regular_expression",
        alias = "regularExpression"
    )]
    RegularExpression { name: String, value: Regex },

    #[serde(untagged)]
    Exact { name: String, value: String },
}

impl HeaderMatch {
    pub fn name(&self) -> &str {
        match self {
            HeaderMatch::RegularExpression { name, .. } => name,
            HeaderMatch::Exact { name, .. } => name,
        }
    }

    pub fn is_match(&self, header_value: &str) -> bool {
        match self {
            HeaderMatch::RegularExpression { value, .. } => value.is_match(header_value),
            HeaderMatch::Exact { value, .. } => value == header_value,
        }
    }
}

/// Describes how to select a HTTP route by matching HTTP query parameters.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields, tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum QueryParamMatch {
    #[serde(
        alias = "regex",
        alias = "regular_expression",
        alias = "regularExpression"
    )]
    RegularExpression { name: String, value: Regex },

    #[serde(untagged)]
    Exact { name: String, value: String },
}

impl QueryParamMatch {
    pub fn name(&self) -> &str {
        match self {
            QueryParamMatch::RegularExpression { name, .. } => name,
            QueryParamMatch::Exact { name, .. } => name,
        }
    }

    pub fn is_match(&self, param_value: &str) -> bool {
        match self {
            QueryParamMatch::RegularExpression { value, .. } => value.is_match(param_value),
            QueryParamMatch::Exact { value, .. } => value == param_value,
        }
    }
}

/// Describes how to select a HTTP route by matching the HTTP method as defined by [RFC
/// 7231](https://datatracker.ietf.org/doc/html/rfc7231#section-4) and [RFC
/// 5789](https://datatracker.ietf.org/doc/html/rfc5789#section-2). The value is expected in upper
/// case.
pub type Method = String;

/// Defines processing steps that must be completed during the request or response lifecycle.
//
// TODO: This feels very gateway-ey and redundant to type out in config. Should we switch to
// untagged here? Something else?
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum RouteFilter {
    /// Defines a schema for a filter that modifies request headers.
    RequestHeaderModifier {
        /// A Header filter.
        #[serde(alias = "requestHeaderModifier")]
        request_header_modifier: HeaderFilter,
    },

    ///  Defines a schema for a filter that modifies response headers.
    ResponseHeaderModifier {
        /// A Header filter.
        #[serde(alias = "responseHeaderModifier")]
        response_header_modifier: HeaderFilter,
    },

    /// Defines a schema for a filter that mirrors requests. Requests are sent to the specified
    /// destination, but responses from that destination are ignored.
    ///
    /// This filter can be used multiple times within the same rule. Note that not all
    /// implementations will be able to support mirroring to multiple backends.
    RequestMirror {
        #[serde(alias = "requestMirror")]
        request_mirror: RequestMirrorFilter,
    },

    /// Defines a schema for a filter that responds to the request with an HTTP redirection.
    RequestRedirect {
        #[serde(alias = "requestRedirect")]
        /// A redirect filter.
        request_redirect: RequestRedirectFilter,
    },

    /// Defines a schema for a filter that modifies a request during forwarding.
    URLRewrite {
        /// A URL rewrite filter.
        #[serde(alias = "urlRewrite")]
        url_rewrite: UrlRewriteFilter,
    },
}

/// Defines configuration for the RequestHeaderModifier filter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct HeaderFilter {
    /// Overwrites the request with the given header (name, value) before the action. Note that the
    /// header names are case-insensitive (see
    /// <https://datatracker.ietf.org/doc/html/rfc2616#section-4.2>).
    ///
    /// Input: GET /foo HTTP/1.1 my-header: foo
    ///
    /// Config: set:
    ///   - name: "my-header" value: "bar"
    ///
    /// Output: GET /foo HTTP/1.1 my-header: bar
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<HeaderValue>,

    /// Add adds the given header(s) (name, value) to the request before the action. It appends to
    /// any existing values associated with the header name.
    ///
    /// Input: GET /foo HTTP/1.1 my-header: foo
    ///
    /// Config: add:
    ///   - name: "my-header" value: "bar"
    ///
    /// Output: GET /foo HTTP/1.1 my-header: foo my-header: bar
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<HeaderValue>,

    /// Remove the given header(s) from the HTTP request before the action. The value of Remove is a
    /// list of HTTP header names. Note that the header names are case-insensitive (see
    /// <https://datatracker.ietf.org/doc/html/rfc2616#section-4.2>).
    ///
    /// Input: GET /foo HTTP/1.1 my-header1: foo my-header2: bar my-header3: baz
    ///
    /// Config: remove: ["my-header1", "my-header3"]
    ///
    /// Output: GET /foo HTTP/1.1 my-header2: bar
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct HeaderValue {
    /// The name of the HTTP Header. Note header names are case insensitive. (See
    /// <https://tools.ietf.org/html/rfc7230#section-3.2>).
    pub name: HeaderName,

    /// The value of HTTP Header.
    pub value: String,
}

/// Defines configuration for path modifiers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum PathModifier {
    /// Specifies the value with which to replace the full path of a request during a rewrite or
    /// redirect.
    ReplaceFullPath {
        /// The value to replace the path with.
        #[serde(alias = "replaceFullPath")]
        replace_full_path: String,
    },

    /// Specifies the value with which to replace the prefix match of a request during a rewrite or
    /// redirect. For example, a request to "/foo/bar" with a prefix match of "/foo" and a
    /// ReplacePrefixMatch of "/xyz" would be modified to "/xyz/bar".
    ///
    /// Note that this matches the behavior of the PathPrefix match type. This matches full path
    /// elements. A path element refers to the list of labels in the path split by the `/`
    /// separator. When specified, a trailing `/` is ignored. For example, the paths `/abc`,
    /// `/abc/`, and `/abc/def` would all match the prefix `/abc`, but the path `/abcd` would not.
    ///
    /// ReplacePrefixMatch is only compatible with a `PathPrefix` route match::
    ///
    ///  ```plaintext,no_run
    ///  Request Path | Prefix Match | Replace Prefix | Modified Path
    ///  -------------|--------------|----------------|----------
    ///  /foo/bar     | /foo         | /xyz           | /xyz/bar
    ///  /foo/bar     | /foo         | /xyz/          | /xyz/bar
    ///  /foo/bar     | /foo/        | /xyz           | /xyz/bar
    ///  /foo/bar     | /foo/        | /xyz/          | /xyz/bar
    ///  /foo         | /foo         | /xyz           | /xyz
    ///  /foo/        | /foo         | /xyz           | /xyz/
    ///  /foo/bar     | /foo         | <empty string> | /bar
    ///  /foo/        | /foo         | <empty string> | /
    ///  /foo         | /foo         | <empty string> | /
    ///  /foo/        | /foo         | /              | /
    ///  /foo         | /foo         | /              | /
    ///  ```
    ReplacePrefixMatch {
        #[serde(alias = "replacePrefixMatch")]
        replace_prefix_match: String,
    },
}

/// Defines a filter that redirects a request. This filter MUST not be used on the same Route rule
/// as a URL Rewrite filter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RequestRedirectFilter {
    /// The scheme to be used in the value of the `Location` header in the response. When empty, the
    /// scheme of the request is used.
    ///
    /// Scheme redirects can affect the port of the redirect, for more information, refer to the
    /// documentation for the port field of this filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,

    /// The hostname to be used in the value of the `Location` header in the response. When empty,
    /// the hostname in the `Host` header of the request is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<PreciseHostname>,

    /// Defines parameters used to modify the path of the incoming request. The modified path is
    /// then used to construct the `Location` header. When empty, the request path is used as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathModifier>,

    /// The port to be used in the value of the `Location` header in the response.
    ///
    /// If no port is specified, the redirect port MUST be derived using the following rules:
    ///
    /// * If redirect scheme is not-empty, the redirect port MUST be the well-known port associated
    ///   with the redirect scheme. Specifically "http" to port 80 and "https" to port 443. If the
    ///   redirect scheme does not have a well-known port, the listener port of the Gateway SHOULD
    ///   be used.
    /// * If redirect scheme is empty, the redirect port MUST be the Gateway Listener port.
    ///
    /// Will not add the port number in the 'Location' header in the following cases:
    ///
    /// * A Location header that will use HTTP (whether that is determined via the Listener protocol
    ///   or the Scheme field) _and_ use port 80.
    /// * A Location header that will use HTTPS (whether that is determined via the Listener
    ///   protocol or the Scheme field) _and_ use port 443.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,

    /// The HTTP status code to be used in response.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "statusCode")]
    pub status_code: Option<u16>,
}

/// Defines a filter that modifies a request during forwarding. At most one of these filters may be
/// used on a Route rule. This may not be used on the same Route rule as a RequestRedirect filter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct UrlRewriteFilter {
    /// The value to be used to replace the Host header value during forwarding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<PreciseHostname>,

    /// Defines a path rewrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathModifier>,
}

/// Defines configuration for the RequestMirror filter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RequestMirrorFilter {
    /// Represents the percentage of requests that should be mirrored to BackendRef. Its minimum
    /// value is 0 (indicating 0% of requests) and its maximum value is 100 (indicating 100% of
    /// requests).
    ///
    /// Only one of Fraction or Percent may be specified. If neither field is specified, 100% of
    /// requests will be mirrored.
    pub percent: Option<i32>,

    /// Only one of Fraction or Percent may be specified. If neither field is specified, 100% of
    /// requests will be mirrored.
    pub fraction: Option<Fraction>,

    pub backend: Target,
}

/// Specifies a way of configuring client retry policy.
///
/// Modelled on the forthcoming [Gateway API type](https://gateway-api.sigs.k8s.io/geps/gep-1731/).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteRetry {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<Duration>,
}

const fn default_weight() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct WeightedTarget {
    #[serde(default = "default_weight")]
    pub weight: u32,

    #[serde(flatten)]
    pub target: Target,
    //Todo: gateway API also allows filters here under an extended support condition we need to
    // decide whether this is one where its simpler just to drop it.
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::*;
    use crate::{
        http::{HeaderMatch, RouteRule},
        shared::Regex,
        DNSTarget, ServiceTarget, Target,
    };

    #[test]
    fn test_passthrough_route() {
        let route = Route::passthrough_route(Target::DNS(DNSTarget {
            hostname: "example.com".to_string(),
            port: None,
        }));
        assert!(route.is_passthrough_route())
    }

    #[test]
    fn test_header_matcher() {
        let test_json = json!([
            { "name":"bar", "type" : "RegularExpression", "value": ".*foo"},
            { "name":"bar", "value": "a literal"},
        ]);
        let obj: Vec<HeaderMatch> = serde_json::from_value(test_json.clone()).unwrap();

        assert_eq!(
            obj,
            vec![
                HeaderMatch::RegularExpression {
                    name: "bar".to_string(),
                    value: Regex::from_str(".*foo").unwrap(),
                },
                HeaderMatch::Exact {
                    name: "bar".to_string(),
                    value: "a literal".to_string(),
                }
            ]
        );

        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }

    #[test]
    fn parses_retry_policy() {
        let test_json = json!({
            "codes":[ 1, 2 ],
            "attempts": 3,
            "backoff": "1m"
        });
        let obj: RouteRetry = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(obj).unwrap();
        assert_eq!(test_json, output_json);
    }

    #[test]
    fn parses_http_route() {
        let test_json = json!({
            "matches":[
                {
                    "method": "GET",
                    "path": { "value": "foo" },
                    "headers": [
                        {"name":"ian", "value": "foo"},
                        {"name": "bar", "type":"RegularExpression", "value": ".*foo"}
                    ]
                },
                {
                    "query_params": [
                        {"name":"ian", "value": "foo"},
                        {"name": "bar", "type":"RegularExpression", "value": ".*foo"}
                    ]
                }
            ],
            "filters":[{
                "type": "URLRewrite",
                "url_rewrite":{
                    "hostname":"ian.com",
                    "path": {"type":"ReplacePrefixMatch", "replace_prefix_match":"/"}
                }
            }],
            "backends":[
                { "weight": 1, "name": "timeout-svc", "namespace": "foo" }
            ],
            "timeouts": {
                "request": "1s"
            }
        });
        let obj: RouteRule = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }

    #[test]
    fn minimal_route() {
        assert_deserialize(
            json!({
                "target": { "name": "foo", "namespace": "bar" },
                "rules": [
                    {
                        "backends": [ { "name": "foo", "namespace": "bar" } ],
                    }
                ]
            }),
            Route {
                target: Target::Service(ServiceTarget {
                    name: "foo".to_string(),
                    namespace: "bar".to_string(),
                    ..Default::default()
                }),
                rules: vec![RouteRule {
                    matches: vec![],
                    filters: vec![],
                    timeouts: None,
                    retry: None,
                    backends: vec![WeightedTarget {
                        target: Target::Service(ServiceTarget {
                            name: "foo".to_string(),
                            namespace: "bar".to_string(),
                            ..Default::default()
                        }),
                        weight: 1,
                    }],
                }],
            },
        );
    }

    #[test]
    fn minimal_route_missing_target() {
        assert_deserialize_err::<Route>(json!({
            "hostnames": ["foo.bar"],
            "rules": [
                {
                    "matches": [],
                }
            ]
        }));
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
