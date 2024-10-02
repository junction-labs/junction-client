use crate::shared::{
    Duration, Fraction, PortNumber, PreciseHostname, Regex, SessionAffinity, Target, WeightedTarget,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Route {
    /// The target for this route.
    pub target: Target,

    /// The route rules that determine whether any URLs match.
    pub rules: Vec<RouteRule>,
}

impl Route {
    // FIXME: impl Default and make sure this passes on it
    #[doc(hidden)]
    pub fn is_default_route(&self) -> bool {
        self.rules.len() == 1
            && self.rules[0].backends.len() == 1
            && self.rules[0].matches.len() == 1
            && self.rules[0].matches[0].method.is_none()
            && self.rules[0].matches[0].headers.is_empty()
            && self.rules[0].matches[0].query_params.is_empty()
            && self.rules[0].matches[0].path
                == Some(PathMatch::Prefix {
                    value: "".to_string(),
                })
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
    /// For example, take the following matches configuration:
    ///
    /// ```yaml
    /// matches:
    /// - path:
    ///     value: "/foo"
    ///   headers:
    ///   - name: "version"
    ///     value: "v2"
    /// - path:
    ///     value: "/v2/foo"
    /// ```
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

    #[doc(hidden)]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "sessionAffinity"
    )]
    pub session_affinity: Option<SessionAffinity>,

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
/// it contains the `version: v1` header:
///
/// ```yaml
/// match:
///   path:
///     value: "/foo"
///   headers:
///   - name: "version"
///     value "v1"
/// ```
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
    /// ReplacePrefixMatch is only compatible with a `PathPrefix` route match.
    ///
    /// Request Path | Prefix Match | Replace Prefix | Modified Path
    /// -------------|--------------|----------------|----------
    /// /foo/bar     | /foo         | /xyz           | /xyz/bar
    /// /foo/bar     | /foo         | /xyz/          | /xyz/bar
    /// /foo/bar     | /foo/        | /xyz           | /xyz/bar
    /// /foo/bar     | /foo/        | /xyz/          | /xyz/bar
    /// /foo         | /foo         | /xyz           | /xyz
    /// /foo/        | /foo         | /xyz           | /xyz/
    /// /foo/bar     | /foo         | <empty string> | /bar
    /// /foo/        | /foo         | <empty string> | /
    /// /foo         | /foo         | <empty string> | /
    /// /foo/        | /foo         | /              | /
    /// /foo         | /foo         | /              | /
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
/// ( Modelled on the forthcoming Gateway API type https://gateway-api.sigs.k8s.io/geps/gep-1731/ )
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
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

///
/// Now get into xDS deserialization for the above
///
use xds_api::pb::envoy::{
    config::route::v3::{self as xds_route, query_parameter_matcher::QueryParameterMatchSpecifier},
    r#type::matcher::v3::string_matcher::MatchPattern,
};

impl Route {
    pub fn from_xds(xds: &xds_route::RouteConfiguration) -> Result<Self, crate::xds::Error> {
        let Some(target) = Target::from_listener_xds_name(&xds.name) else {
            return Err(crate::xds::Error::InvalidXds {
                resource_type: "Listener",
                resource_name: xds.name.clone(),
                message: "Unable to parse name".to_string(),
            });
        };
        let mut rules = Vec::new();
        for virtual_host in &xds.virtual_hosts {
            // todo: as you see, at the moment we totally quash virtual_host and its domains. When
            // we bring them back for gateways it means is we will need to return a vector of
            // routes, as the rules are within them
            for route in &virtual_host.routes {
                rules.push(RouteRule::from_xds(route)?);
            }
        }
        Ok(Route {
            target: target,
            rules,
        })
    }
}

impl RouteRule {
    pub fn from_xds(route: &xds_route::Route) -> Result<Self, crate::xds::Error> {
        let Some(matches) = route.r#match.as_ref().and_then(RouteMatch::from_xds) else {
            return Err(crate::xds::Error::InvalidXds {
                resource_type: "Route",
                resource_name: route.name.to_string(),
                message: "Unable to parse route".to_string(),
            });
        };
        let Some(xds_route::route::Action::Route(action)) = route.action.as_ref() else {
            return Err(crate::xds::Error::InvalidXds {
                resource_type: "Route",
                resource_name: route.name.to_string(),
                message: "Unable to parse route action".to_string(),
            });
        };

        let timeouts: Option<RouteTimeouts> = RouteTimeouts::from_xds(action);

        let retry = action.retry_policy.as_ref().map(RouteRetry::from_xds);

        let session_affinity = SessionAffinity::from_xds(&action.hash_policy);

        let backends = WeightedTarget::from_xds(action.cluster_specifier.as_ref())?;

        Ok(RouteRule {
            matches: vec![matches],
            retry,
            filters: vec![],
            session_affinity,
            timeouts,
            backends,
        })
    }
}

impl RouteTimeouts {
    pub fn from_xds(r: &xds_route::RouteAction) -> Option<Self> {
        let request = r.timeout.clone().map(|x| x.try_into().unwrap());
        let mut backend_request: Option<Duration> = None;
        if let Some(r1) = &r.retry_policy {
            backend_request = r1.per_try_timeout.clone().map(|x| x.try_into().unwrap());
        }
        if request.is_some() || backend_request.is_some() {
            Some(RouteTimeouts {
                backend_request,
                request,
            })
        } else {
            None
        }
    }
}

impl RouteMatch {
    pub fn from_xds(r: &xds_route::RouteMatch) -> Option<Self> {
        let path = r.path_specifier.as_ref().and_then(PathMatch::from_xds);

        // with xds, any method match is converted into a match on a ":method" header. it does not
        // have a way of specifying a method match otherwise. to keep the "before and after" xDS as
        // similar as possible then we pull this out of the headers list if it exists
        let mut method: Option<Method> = None;
        let mut headers = vec![];
        for header in &r.headers {
            let Some(header_match) = HeaderMatch::from_xds(header) else {
                continue;
            };

            match header_match {
                HeaderMatch::Exact { name, value } if name == ":method" => {
                    method = Some(value);
                }
                _ => {
                    headers.push(header_match);
                }
            }
        }

        let query_params = r
            .query_parameters
            .iter()
            .filter_map(QueryParamMatch::from_xds)
            .collect();

        Some(RouteMatch {
            headers,
            method,
            path,
            query_params,
        })
    }
}

fn parse_xds_regex(p: &xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher) -> Option<Regex> {
    //FIXME: check the regex type
    match Regex::from_str(&p.regex) {
        Ok(e) => Some(e),
        Err(_) => None, //FIXME: log/record we can't parse the regex
    }
}

impl QueryParamMatch {
    pub fn from_xds(matcher: &xds_route::QueryParameterMatcher) -> Option<Self> {
        let name = matcher.name.clone();
        match matcher.query_parameter_match_specifier.as_ref()? {
            QueryParameterMatchSpecifier::StringMatch(s) => match s.match_pattern.as_ref() {
                Some(MatchPattern::Exact(s)) => Some(QueryParamMatch::Exact {
                    name,
                    value: s.clone(),
                }),
                Some(MatchPattern::SafeRegex(pfx)) => Some(QueryParamMatch::RegularExpression {
                    name,
                    value: parse_xds_regex(pfx)?,
                }),
                Some(_) | None => {
                    //fixme: raise an error that config is being thrown away
                    None
                }
            },
            _ => {
                // FIXME: log/record that we are throwing away config
                None
            }
        }
    }
}

impl HeaderMatch {
    fn from_xds(header_matcher: &xds_route::HeaderMatcher) -> Option<Self> {
        let name = header_matcher.name.clone();
        match header_matcher.header_match_specifier.as_ref()? {
            xds_route::header_matcher::HeaderMatchSpecifier::ExactMatch(value) => {
                Some(HeaderMatch::Exact {
                    name,
                    value: value.clone(),
                })
            }
            xds_route::header_matcher::HeaderMatchSpecifier::SafeRegexMatch(regex) => {
                Some(HeaderMatch::RegularExpression {
                    name,
                    value: parse_xds_regex(regex)?,
                })
            }
            _ => {
                // FIXME: log/record that we are throwing away config
                None
            }
        }
    }
}

impl PathMatch {
    fn from_xds(path_spec: &xds_route::route_match::PathSpecifier) -> Option<Self> {
        match path_spec {
            xds_route::route_match::PathSpecifier::Prefix(p) => {
                Some(PathMatch::Prefix { value: p.clone() })
            }
            xds_route::route_match::PathSpecifier::Path(p) => {
                Some(PathMatch::Exact { value: p.clone() })
            }
            xds_route::route_match::PathSpecifier::SafeRegex(p) => {
                Some(PathMatch::RegularExpression {
                    value: parse_xds_regex(p)?,
                })
            }
            _ => {
                // FIXME: log/record that we are throwing away config
                None
            }
        }
    }
}

impl RouteRetry {
    pub fn from_xds(r: &xds_route::RetryPolicy) -> Self {
        let codes = r.retriable_status_codes.clone();
        let attempts = Some(1 + r.num_retries.clone().map_or(0, |v| v.into()));
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
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::{Route, RouteRetry};
    use crate::{
        http::{HeaderMatch, RouteRule, SessionAffinity},
        shared::{
            Regex, ServiceTarget, SessionAffinityHashParam, SessionAffinityHashParamType, Target,
            WeightedTarget,
        },
    };

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
                    session_affinity: None,
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

    #[test]
    fn session_affinity() {
        assert_deserialize(
            json!({
                "hashParams": [
                    {"type": "Header", "name": "x-foo"},
                    {"type": "Header", "name": "x-foo2", "terminal": true}
                    ],
            }),
            SessionAffinity {
                hash_params: vec![
                    SessionAffinityHashParam {
                        matcher: SessionAffinityHashParamType::Header {
                            name: "x-foo".to_string(),
                        },
                        terminal: false,
                    },
                    SessionAffinityHashParam {
                        matcher: SessionAffinityHashParamType::Header {
                            name: "x-foo2".to_string(),
                        },
                        terminal: true,
                    },
                ],
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
