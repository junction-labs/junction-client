use crate::shared::{Duration, PortNumber, PreciseHostname, Regex, StringMatch};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    // fixme: here is where the gateway API allows a Vec<ParentRef>, and we
    // likely we will need something like it
    //
    /// The domains that this applies to. Domains are matched against the
    /// incoming authority of any URL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostnames: Vec<String>,

    /// The route rules that determine whether any URLs match.
    pub rules: Vec<RouteRule>,
}

/// Defines semantics for matching an HTTP request based on conditions
/// (matches), processing it (filters), and forwarding the request to an API
/// object (backendRefs).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteRule {
    /// Defines conditions used for matching the rule against incoming HTTP
    /// requests. Each match is independent, i.e. this rule will be matched if
    /// **any** one of the matches is satisfied.
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
    /// For a request to match against this rule, a request must satisfy EITHER
    /// of the two conditions:
    ///
    /// - path prefixed with `/foo` AND contains the header `version: v2`
    /// - path prefix of `/v2/foo`
    ///
    /// See the documentation for RouteMatch on how to specify multiple match
    /// conditions that should be ANDed together.
    ///
    /// If no matches are specified, the default is a prefix path match on "/",
    /// which has the effect of matching every HTTP request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<RouteMatch>,

    /// Define the filters that are applied to requests that match this rule.
    ///
    /// The effects of ordering of multiple behaviors are currently unspecified.
    ///
    /// Specifying the same filter multiple times is not supported unless
    /// explicitly indicated in the filter.
    ///
    /// All filters are compatible with each other except for the URLRewrite and
    /// RequestRedirect filters, which may not be combined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<RouteFilter>,

    //todo: enable session persistence
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_persistence: Option<SessionPersistence>,

    // Timeouts defines the timeouts that can be configured for an HTTP request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<RouteTimeouts>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity: Option<SessionAffinityPolicy>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RouteRetryPolicy>,

    pub target: RouteTarget,
    // this is the CRD Version
    // ```
    // #[serde(default, skip_serializing_if = "Vec::is_empty")]
    // pub backend_refs: Vec<HTTPBackendRef>,
    // ```
}

/// Defines and configures session persistence for the route rule.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionPersistence {
    /// Defines the name of the persistent session token which may be reflected
    /// in the cookie or the header. Avoid reusing session names to prevent
    /// unintended consequences, such as rejection or unpredictable behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,

    /// Defines the absolute timeout of the persistent session. Once the
    /// AbsoluteTimeout duration has elapsed, the session becomes invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute_timeout: Option<String>,

    /// Provides configuration settings that are specific to cookie-based
    /// session persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_config: Option<SessionPersistenceCookieConfig>,

    /// Defines the idle timeout of the persistent session. Once the session has
    /// been idle for more than the specified IdleTimeout duration, the session
    /// becomes invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<Duration>,

    /// Defines the type of session persistence such as through the use a header
    /// or cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<SessionPersistenceType>,
}

/// Provides configuration settings that are specific to cookie-based session
/// persistence.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionPersistenceCookieConfig {
    /// Specifies whether the cookie has a permanent or session-based lifetime.
    /// A permanent cookie persists until its specified expiry time, defined by
    /// the Expires or Max-Age cookie attributes, while a session cookie is
    /// deleted when the current session ends.
    ///
    /// When set to "Permanent", AbsoluteTimeout indicates the cookie's lifetime
    /// via the Expires or Max-Age cookie attributes and is required.
    ///
    /// When set to "Session", AbsoluteTimeout indicates the absolute lifetime
    /// of the cookie and is optional.
    pub lifetime_type: Option<SessionPersistenceCookieLifetimeType>,
}

/// Provides configuration settings that are specific to cookie-based session
/// persistence.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum SessionPersistenceCookieLifetimeType {
    Permanent,
    Session,
}

/// Defines and configures session persistence for the route rule.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum SessionPersistenceType {
    Cookie,
    Header,
}

/// Defines timeouts that can be configured for a http Route. Specifying a zero
/// value such as "0s" is interpreted as no timeout.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteTimeouts {
    /// Specifies a timeout for an individual request from the gateway to a
    /// backend. This covers the time from when the request first starts being
    /// sent from the gateway to when the full response has been received from
    /// the backend.
    ///
    /// An entire client HTTP transaction with a gateway, covered by the Request
    /// timeout, may result in more than one call from the gateway to the
    /// destination backend, for example, if automatic retries are supported.
    ///
    /// Because the Request timeout encompasses the BackendRequest timeout, the
    /// value of BackendRequest must be <= the value of Request timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_request: Option<Duration>,

    /// Specifies the maximum duration for a gateway to respond to an HTTP
    /// request. If the gateway has not been able to respond before this
    /// deadline is met, the gateway MUST return a timeout error.
    ///
    /// For example, setting the `rules.timeouts.request` field to the value
    /// `10s` will cause a timeout if a client request is taking longer than 10
    /// seconds to complete.
    ///
    /// This timeout is intended to cover as close to the whole request-response
    /// transaction as possible although an implementation MAY choose to start
    /// the timeout after the entire request stream has been received instead of
    /// immediately after the transaction is initiated by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Duration>,
}

/// Defines the predicate used to match requests to a given action. Multiple
/// match types are ANDed together, i.e. the match will evaluate to true only if
/// all conditions are satisfied.
///
/// For example, the match below will match a HTTP request only if its path
/// starts with `/foo` AND it contains the `version: v1` header:
///
/// ```yaml
/// match:
///   path:
///     value: "/foo"
///   headers:
///   - name: "version"
///     value "v1"
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteMatch {
    /// Specifies a HTTP request path matcher. If this field is not specified, a
    /// default prefix match on the "/" path is provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathMatch>,

    /// Specifies HTTP request header matchers. Multiple match values are ANDed
    /// together, meaning, a request must match all the specified headers to
    /// select the route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderMatch>,

    /// Specifies HTTP query parameter matchers. Multiple match values are ANDed
    /// together, meaning, a request must match all the specified query
    /// parameters to select the route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_params: Vec<QueryParamMatch>,

    /// Specifies HTTP method matcher. When specified, this route will be
    /// matched only if the request has the specified method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Method>,
}

/// Describes how to select a HTTP route by matching the HTTP request path.
///
/// The `type` specifies the semantics of how HTTP paths should be compared.
/// Valid PathMatchType values are:
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
pub enum PathMatch {
    Prefix {
        value: String,
    },
    RegularExpression {
        value: Regex,
    },
    #[serde(untagged)]
    Exact {
        value: String,
    },
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
/// * ":method" - ":" is an invalid character. This means that HTTP/2 pseudo
///   headers are not currently supported by this type.
/// * "/invalid" - "/" is an invalid character
pub type HeaderName = String;

/// Describes how to select a HTTP route by matching HTTP request headers.
///
/// `name` is the name of the HTTP Header to be matched. Name matching is case
/// insensitive. (See <https://tools.ietf.org/html/rfc7230#section-3.2>).
///
/// If multiple entries specify equivalent header names, only the first entry
/// with an equivalent name WILL be considered for a match. Subsequent entries
/// with an equivalent header name WILL be ignored. Due to the
/// case-insensitivity of header names, "foo" and "Foo" are considered
/// equivalent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HeaderMatch {
    pub name: HeaderName,
    #[serde(flatten)]
    pub value_matcher: StringMatch,
}

/// Describes how to select a HTTP route by matching HTTP query parameters.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueryParamMatch {
    pub name: String,
    #[serde(flatten)]
    pub value_matcher: StringMatch,
}

/// Describes how to select a HTTP route by matching the HTTP method as defined
/// by [RFC 7231](https://datatracker.ietf.org/doc/html/rfc7231#section-4) and
/// [RFC 5789](https://datatracker.ietf.org/doc/html/rfc5789#section-2). The
/// value is expected in upper case.
pub type Method = String;

/// Defines processing steps that must be completed during the request or
/// response lifecycle.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "PascalCase", deny_unknown_fields)]
pub enum RouteFilter {
    /// Defines a schema for a filter that modifies request headers.
    #[serde(rename_all = "camelCase")]
    RequestHeaderModifier {
        request_header_modifier: RequestHeaderFilter,
    },

    ///  Defines a schema for a filter that modifies response headers.
    #[serde(rename_all = "camelCase")]
    ResponseHeaderModifier {
        response_header_modifier: RequestHeaderFilter,
    },

    /// Defines a schema for a filter that mirrors requests. Requests are sent
    /// to the specified destination, but responses from that destination are
    /// ignored.
    ///
    /// This filter can be used multiple times within the same rule. Note that
    /// not all implementations will be able to support mirroring to multiple
    /// backends.
    #[serde(rename_all = "camelCase")]
    RequestMirror { request_mirror: RequestMirrorFilter },

    /// Defines a schema for a filter that responds to the request with an HTTP
    /// redirection.
    #[serde(rename_all = "camelCase")]
    RequestRedirect {
        request_redirect: RequestRedirectFilter,
    },

    /// Defines a schema for a filter that modifies a request during forwarding.
    #[serde(rename_all = "camelCase")]
    URLRewrite { url_rewrite: UrlRewriteFilter },
}

/// Defines configuration for the RequestHeaderModifier filter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestHeaderFilter {
    /// Overwrites the request with the given header (name, value) before the
    /// action. Note that the header names are case-insensitive (see
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

    /// Add adds the given header(s) (name, value) to the request before the
    /// action. It appends to any existing values associated with the header
    /// name.
    ///
    /// Input: GET /foo HTTP/1.1 my-header: foo
    ///
    /// Config: add:
    ///   - name: "my-header" value: "bar"
    ///
    /// Output: GET /foo HTTP/1.1 my-header: foo my-header: bar
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<HeaderValue>,

    /// Remove the given header(s) from the HTTP request before the action. The
    /// value of Remove is a list of HTTP header names. Note that the header
    /// names are case-insensitive (see
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeaderValue {
    /// The name of the HTTP Header. Note header names are case insensitive.
    /// (See <https://tools.ietf.org/html/rfc7230#section-3.2>).
    pub name: HeaderName,

    /// The value of HTTP Header.
    pub value: String,
}

/// Defines configuration for path modifiers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "PascalCase", deny_unknown_fields)]
pub enum PathModifier {
    /// Specifies the value with which to replace the full path of a request
    /// during a rewrite or redirect.
    #[serde(rename_all = "camelCase")]
    ReplaceFullPath { replace_full_path: String },

    /// Specifies the value with which to replace the prefix match of a request
    /// during a rewrite or redirect. For example, a request to "/foo/bar" with
    /// a prefix match of "/foo" and a ReplacePrefixMatch of "/xyz" would be
    /// modified to "/xyz/bar".
    ///
    /// Note that this matches the behavior of the PathPrefix match type. This
    /// matches full path elements. A path element refers to the list of labels
    /// in the path split by the `/` separator. When specified, a trailing `/`
    /// is ignored. For example, the paths `/abc`, `/abc/`, and `/abc/def` would
    /// all match the prefix `/abc`, but the path `/abcd` would not.
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
    #[serde(rename_all = "camelCase")]
    ReplacePrefixMatch { replace_prefix_match: String },
}

/// Defines a filter that redirects a request. This filter MUST not be used on
/// the same Route rule as a URL Rewrite filter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestRedirectFilter {
    /// The scheme to be used in the value of the `Location` header in the
    /// response. When empty, the scheme of the request is used.
    ///
    /// Scheme redirects can affect the port of the redirect, for more
    /// information, refer to the documentation for the port field of this
    /// filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,

    /// The hostname to be used in the value of the `Location` header in the
    /// response. When empty, the hostname in the `Host` header of the request
    /// is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<PreciseHostname>,

    /// Defines parameters used to modify the path of the incoming request. The
    /// modified path is then used to construct the `Location` header. When
    /// empty, the request path is used as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathModifier>,

    /// The port to be used in the value of the `Location` header in the
    /// response.
    ///
    /// If no port is specified, the redirect port MUST be derived using the
    /// following rules:
    ///
    /// * If redirect scheme is not-empty, the redirect port MUST be the
    ///   well-known port associated with the redirect scheme. Specifically
    ///   "http" to port 80 and "https" to port 443. If the redirect scheme does
    ///   not have a well-known port, the listener port of the Gateway SHOULD be
    ///   used.
    /// * If redirect scheme is empty, the redirect port MUST be the Gateway
    ///   Listener port.
    ///
    /// Will not add the port number in the 'Location' header in the following
    /// cases:
    ///
    /// * A Location header that will use HTTP (whether that is determined via
    ///   the Listener protocol or the Scheme field) _and_ use port 80.
    /// * A Location header that will use HTTPS (whether that is determined via
    ///   the Listener protocol or the Scheme field) _and_ use port 443.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,

    /// The HTTP status code to be used in response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

/// Defines a filter that modifies a request during forwarding. At most one of
/// these filters may be used on a Route rule. This may not be used on the same
/// Route rule as a RequestRedirect filter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestMirrorFilter {
    /// Represents the percentage of requests that should be mirrored to
    /// BackendRef. Its minimum value is 0 (indicating 0% of requests) and its
    /// maximum value is 100 (indicating 100% of requests).
    ///
    /// Only one of Fraction or Percent may be specified. If neither field is
    /// specified, 100% of requests will be mirrored.
    pub percent: Option<i32>,

    /// Only one of Fraction or Percent may be specified. If neither field is
    /// specified, 100% of requests will be mirrored.
    pub fraction: Option<(i32, i32)>,

    pub target: RouteTarget,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub enum RouteTarget {
    Cluster(String),
    WeightedClusters(Vec<WeightedCluster>),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WeightedCluster {
    pub name: String,
    pub weight: u32,
}

/// Specifies a way of configuring client retry policy.
///
/// ( Modelled on the forthcoming Gateway API type
/// https://gateway-api.sigs.k8s.io/geps/gep-1731/ )
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteRetryPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<Duration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(tag = "type")]
pub enum SessionAffinityHashParamType {
    Header { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionAffinityHashParam {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
    #[serde(flatten)]
    pub matcher: SessionAffinityHashParamType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionAffinityPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hash_params: Vec<SessionAffinityHashParam>,
}

///
/// Now get into xDS deserialization for the above
///
use xds_api::pb::envoy::{
    config::route::v3::{self as xds_route, query_parameter_matcher::QueryParameterMatchSpecifier},
    r#type::matcher::v3::string_matcher::MatchPattern,
};

impl Route {
    pub fn from_xds(vhost: &xds_route::VirtualHost) -> Self {
        let hostnames = vhost.domains.clone();
        let rules: Vec<_> = vhost
            .routes
            .iter()
            .filter_map(RouteRule::from_xds)
            .collect();

        Route { hostnames, rules }
    }
}

impl RouteRule {
    pub fn from_xds(route: &xds_route::Route) -> Option<Self> {
        let matches: RouteMatch = route.r#match.as_ref().and_then(RouteMatch::from_xds)?;

        let Some(xds_route::route::Action::Route(action)) = route.action.as_ref() else {
            return None; //FIXME: is this really the right thing to do once we support filters?
        };

        let timeouts: Option<RouteTimeouts> = RouteTimeouts::from_xds(action);

        let retry_policy = action.retry_policy.as_ref().map(RouteRetryPolicy::from_xds);

        let session_persistence = None; //FIXME: fill this from xds

        let session_affinity = SessionAffinityPolicy::from_xds(&action.hash_policy);

        let target = RouteTarget::from_xds(action.cluster_specifier.as_ref()?)?;

        Some(RouteRule {
            matches: vec![matches],
            retry_policy,
            filters: vec![],
            session_affinity,
            timeouts,
            target,
            session_persistence,
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
            return Some(RouteTimeouts {
                backend_request: backend_request,
                request: request,
            });
        } else {
            return None;
        }
    }
}

impl RouteMatch {
    pub fn from_xds(r: &xds_route::RouteMatch) -> Option<Self> {
        let path = r.path_specifier.as_ref().and_then(PathMatch::from_xds);

        // with xds, any method match is converted into a match on a ":method"
        // header. it does not have a way of specifying a method match
        // otherwise. to keep the "before and after" xDS as similar as possible
        // then we pull this out of the headers list if it exists
        let mut method: Option<Method> = None;
        let headers = r
            .headers
            .iter()
            .filter_map(HeaderMatch::from_xds)
            .filter({
                |header| {
                    if header.name == ":method" {
                        match &header.value_matcher {
                            StringMatch::Exact { value } => method = Some(value.clone()),
                            _ => {
                                //fixme: we are throwing away config
                            }
                        }
                        true
                    } else {
                        false
                    }
                }
            })
            .collect();

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
        let value_matcher = match matcher.query_parameter_match_specifier.as_ref()? {
            QueryParameterMatchSpecifier::StringMatch(s) => match s.match_pattern.as_ref() {
                Some(MatchPattern::Exact(s)) => {
                    //FIXME: if case insensitive is set, should convert to a
                    //regex or maybe just throw an error
                    StringMatch::Exact { value: s.clone() }
                }
                Some(MatchPattern::SafeRegex(pfx)) => {
                    let Some(x) = parse_xds_regex(pfx) else {
                        return None;
                    };
                    StringMatch::RegularExpression { value: x }
                }
                Some(_) | None => {
                    //fixme: raise an error that config is being thrown away
                    return None;
                }
            },
            _ => {
                // FIXME: log/record that we are throwing away config
                return None;
            }
        };
        let name = matcher.name.clone();
        Some(QueryParamMatch {
            name,
            value_matcher,
        })
    }
}

impl HeaderMatch {
    fn from_xds(header_matcher: &xds_route::HeaderMatcher) -> Option<Self> {
        let name = header_matcher.name.clone();
        let value_matcher = match header_matcher.header_match_specifier.as_ref()? {
            xds_route::header_matcher::HeaderMatchSpecifier::ExactMatch(s) => {
                StringMatch::Exact { value: s.clone() }
            }
            xds_route::header_matcher::HeaderMatchSpecifier::SafeRegexMatch(pfx) => {
                let Some(x) = parse_xds_regex(pfx) else {
                    return None;
                };
                StringMatch::RegularExpression { value: x }
            }
            _ => {
                // FIXME: log/record that we are throwing away config
                return None;
            }
        };

        Some(HeaderMatch {
            name,
            value_matcher,
        })
    }
}

impl PathMatch {
    fn from_xds(path_spec: &xds_route::route_match::PathSpecifier) -> Option<Self> {
        return match path_spec {
            xds_route::route_match::PathSpecifier::Prefix(p) => {
                Some(PathMatch::Prefix { value: p.clone() })
            }
            xds_route::route_match::PathSpecifier::Path(p) => {
                Some(PathMatch::Exact { value: p.clone() })
            }
            xds_route::route_match::PathSpecifier::SafeRegex(p) => {
                let Some(x) = parse_xds_regex(p) else {
                    return None;
                };
                Some(PathMatch::RegularExpression { value: x })
            }
            _ => {
                // FIXME: log/record that we are throwing away config
                None
            }
        };
    }
}

impl RouteRetryPolicy {
    pub fn from_xds(r: &xds_route::RetryPolicy) -> Self {
        let codes = r.retriable_status_codes.clone();
        let attempts = Some(1 + r.num_retries.clone().map_or(0, |v| v.into()) as usize);
        let backoff = r
            .retry_back_off
            .as_ref()
            .map(|r2| r2.base_interval.clone().map(|x| x.try_into().unwrap()))
            .flatten();
        Self {
            codes,
            attempts,
            backoff,
        }
    }
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
                        weight: crate::value_or_default!(w.weight, 0),
                    })
                    .collect();

                Some(Self::WeightedClusters(named_and_weights))
            }
            _ => None,
        }
    }
}

impl SessionAffinityHashParam {
    //only returns session affinity
    pub fn from_xds(hash_policy: &xds_route::route_action::HashPolicy) -> Option<Self> {
        use xds_route::route_action::hash_policy::PolicySpecifier;

        match hash_policy.policy_specifier.as_ref() {
            Some(PolicySpecifier::Header(h)) => Some(SessionAffinityHashParam {
                terminal: hash_policy.terminal,
                matcher: SessionAffinityHashParamType::Header {
                    name: h.header_name.clone(),
                },
            }),
            _ => {
                //FIXME; thrown away config
                None
            }
        }
    }
}

impl SessionAffinityPolicy {
    //only returns session affinity
    pub fn from_xds(hash_policy: &Vec<xds_route::route_action::HashPolicy>) -> Option<Self> {
        let hash_params: Vec<_> = hash_policy
            .iter()
            .filter_map(SessionAffinityHashParam::from_xds)
            .collect();

        if hash_params.len() == 0 {
            return None;
        } else {
            return Some(SessionAffinityPolicy { hash_params });
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::{
        Route, RouteRetryPolicy, RouteTarget, SessionAffinityHashParam,
        SessionAffinityHashParamType,
    };
    use crate::http::{HeaderMatch, RouteRule, SessionAffinityPolicy};

    #[test]
    fn test_string_matcher() {
        let test_json = json!([
            { "name":"bar", "type" : "RegularExpression", "value": ".*foo" },
            { "name":"bar", "value": ".*foo" },
        ]);
        let obj: Vec<HeaderMatch> = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }

    #[test]
    fn parses_session_affinity_policy() {
        let test_json = json!({
            "hashParams": [
                { "type": "Header", "name": "FOO",  "terminal": true },
                { "type": "Header", "name": "FOO"}
            ]
        });
        let obj: SessionAffinityPolicy = serde_json::from_value(test_json.clone()).unwrap();
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
        let obj: RouteRetryPolicy = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
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
                        { "name":"ian", "value": "foo" },
                        { "type":"RegularExpression", "name":"bar", "value": ".*foo" }
                    ]
                },
                {
                    "queryParams": [
                        { "name":"ian","value": "foo" },
                        { "type":"RegularExpression", "name":"bar", "value": ".*foo" }
                    ]
                }
            ],
            "filters":[{
                "type":"URLRewrite",
                "urlRewrite":{
                    "hostname":"ian.com",
                    "path":{ "type":"ReplacePrefixMatch", "replacePrefixMatch":"/" }
                }
            }],
            "target":[
                { "weight": 1, "name": "timeout-svc" }
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
                "hostnames": ["foo.bar"],
                "rules": [
                    {
                        "target": "foo.bar",
                    }
                ]
            }),
            Route {
                hostnames: vec!["foo.bar".to_string()],
                rules: vec![RouteRule {
                    matches: vec![],
                    filters: vec![],
                    timeouts: None,
                    session_persistence: None,
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
            SessionAffinityPolicy {
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
