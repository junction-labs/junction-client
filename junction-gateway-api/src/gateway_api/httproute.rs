use super::{duration::Duration, gateway_api_shared::*};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// HTTPRouteRule defines semantics for matching an HTTP request based on
/// conditions (matches), processing it (filters), and forwarding the request to
/// an API object (backendRefs).
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HTTPRouteRule {
    /// Matches define conditions used for matching the rule against incoming
    /// HTTP requests. Each match is independent, i.e. this rule will be matched
    /// if **any** one of the matches is satisfied.
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
    /// For a request to match against this rule, a request must satisfy
    /// EITHER of the two conditions:
    ///
    /// - path prefixed with `/foo` AND contains the header `version: v2`
    /// - path prefix of `/v2/foo`
    ///
    /// See the documentation for HTTPRouteMatch on how to specify multiple
    /// match conditions that should be ANDed together.
    ///
    /// If no matches are specified, the default is a prefix
    /// path match on "/", which has the effect of matching every
    /// HTTP request.
    ///
    /// Proxy or Load Balancer routing configuration generated from HTTPRoutes
    /// MUST prioritize matches based on the following criteria, continuing on
    /// ties. Across all rules specified on applicable Routes, precedence must be
    /// given to the match having:
    ///
    /// * "Exact" path match.
    /// * "Prefix" path match with largest number of characters.
    /// * Method match.
    /// * Largest number of header matches.
    /// * Largest number of query param matches.
    ///
    /// Note: The precedence of RegularExpression path matches are implementation-specific.
    ///
    /// If ties still exist across multiple Routes, matching precedence MUST be
    /// determined in order of the following criteria, continuing on ties:
    ///
    /// * For those routes set by k8es: The oldest Route based on creation timestamp.
    /// * The Route appearing first in alphabetical order by
    ///   "{namespace}/{name}".
    ///
    /// If ties still exist within an HTTPRoute, matching precedence MUST be granted
    /// to the FIRST matching rule (in list order) with a match meeting the above
    /// criteria.
    ///
    /// When no rules matching a request have been successfully attached to the
    /// parent a request is coming from, a HTTP 404 status code MUST be returned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<HTTPRouteMatch>,

    /// Filters define the filters that are applied to requests that match
    /// this rule.
    ///
    /// The effects of ordering of multiple behaviors are currently unspecified.
    /// This can change in the future based on feedback during the alpha stage.
    ///
    /// Specifying the same filter multiple times is not supported unless explicitly
    /// indicated in the filter.
    ///
    /// All filters are expected to be compatible with each other except for the
    /// URLRewrite and RequestRedirect filters, which may not be combined. If an
    /// implementation can not support other combinations of filters, they must clearly
    /// document that limitation. In cases where incompatible or unsupported
    /// filters are specified and cause the `Accepted` condition to be set to status
    /// `False`, implementations may use the `IncompatibleFilters` reason to specify
    /// this configuration error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<HTTPRouteFilter>,

    //todo: enable session persistence
    //#[serde(default, skip_serializing_if = "Option::is_none")]
    //pub session_persistence: Option<HTTPSessionPersistence>,

    // Timeouts defines the timeouts that can be configured for an HTTP request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<HTTPRouteTimeouts>,

    /// BackendRefs defines the backend(s) where matching requests should be
    /// sent.
    ///
    /// Failure behavior here depends on how many BackendRefs are specified and
    /// how many are invalid.
    ///
    /// If *all* entries in BackendRefs are invalid, and there are also no filters
    /// specified in this route rule, *all* traffic which matches this rule MUST
    /// receive a 500 status code.
    ///
    /// See the HTTPBackendRef definition for the rules about what makes a single
    /// HTTPBackendRef invalid.
    ///
    /// When a HTTPBackendRef is invalid, 500 status codes MUST be returned for
    /// requests that would have otherwise been routed to an invalid backend. If
    /// multiple backends are specified, and some are invalid, the proportion of
    /// requests that would otherwise have been routed to an invalid backend
    /// MUST receive a 500 status code.
    ///
    /// For example, if two backends are specified with equal weights, and one is
    /// invalid, 50 percent of traffic must receive a 500. Implementations may
    /// choose how that 50 percent is determined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backend_refs: Vec<HTTPBackendRef>,
}

/// HTTPRouteTimeouts defines timeouts that can be configured for an HTTPRoute.
/// Timeout values are represented with Gateway API Duration formatting.
/// Specifying a zero value such as "0s" is interpreted as no timeout.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HTTPRouteTimeouts {
    /// BackendRequest specifies a timeout for an individual request from the gateway
    /// to a backend. This covers the time from when the request first starts being
    /// sent from the gateway to when the full response has been received from the backend.
    ///
    /// An entire client HTTP transaction with a gateway, covered by the Request timeout,
    /// may result in more than one call from the gateway to the destination backend,
    /// for example, if automatic retries are supported.
    ///
    /// Because the Request timeout encompasses the BackendRequest timeout, the value of
    /// BackendRequest must be <= the value of Request timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_request: Option<super::duration::Duration>,

    /// Request specifies the maximum duration for a gateway to respond to an HTTP request.
    /// If the gateway has not been able to respond before this deadline is met, the gateway
    /// MUST return a timeout error.
    ///
    /// For example, setting the `rules.timeouts.request` field to the value `10s` in an
    /// `HTTPRoute` will cause a timeout if a client request is taking longer than 10 seconds
    /// to complete.
    ///
    /// This timeout is intended to cover as close to the whole request-response transaction
    /// as possible although an implementation MAY choose to start the timeout after the entire
    /// request stream has been received instead of immediately after the transaction is
    /// initiated by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<super::duration::Duration>,
}

/// HTTPRouteMatch defines the predicate used to match requests to a given
/// action. Multiple match types are ANDed together, i.e. the match will
/// evaluate to true only if all conditions are satisfied.
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
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HTTPRouteMatch {
    /// Path specifies a HTTP request path matcher. If this field is not
    /// specified, a default prefix match on the "/" path is provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<HTTPPathMatch>,

    /// Headers specifies HTTP request header matchers. Multiple match values
    /// are ANDed together, meaning, a request must match all the specified
    /// headers to select the route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HTTPHeaderMatch>,

    /// QueryParams specifies HTTP query parameter matchers. Multiple match
    /// values are ANDed together, meaning, a request must match all the
    /// specified query parameters to select the route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_params: Vec<HTTPQueryParamMatch>,

    /// Method specifies HTTP method matcher.
    /// When specified, this route will be matched only if the request has the
    /// specified method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<HTTPMethod>,
}

/// HTTPPathMatch describes how to select a HTTP route by matching the HTTP request path.
///
/// The `type` specifies the semantics of how HTTP paths should be compared.
/// Valid PathMatchType values are:
///
/// * "Exact"
/// * "PathPrefix"
/// * "RegularExpression"
///
/// PathPrefix and Exact paths must be syntactically valid:
///
/// - Must begin with the `/` character
/// - Must not contain consecutive `/` characters (e.g. `/foo///`, `//`)
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
pub enum HTTPPathMatchType {
    #[default]
    Exact,
    PathPrefix,
    RegularExpression,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HTTPPathMatch {
    #[serde(default, skip_serializing_if = "is_default")]
    pub r#type: HTTPPathMatchType,
    pub value: String,
}

///
/// Little helper method to skip serializing the defaul value of enums
///
fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}

/// HTTPHeaderName is the name of an HTTP header.
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
pub type HTTPHeaderName = String;

/// HTTPHeaderMatch describes how to select a HTTP route by matching HTTP
/// request headers.
///
/// `name` is the name of the HTTP Header to be matched. Name matching MUST be
/// case insensitive. (See <https://tools.ietf.org/html/rfc7230#section-3.2>).
///
/// If multiple entries specify equivalent header names, only the first
/// entry with an equivalent name WILL be considered for a match. Subsequent
/// entries with an equivalent header name WILL be ignored. Due to the
/// case-insensitivity of header names, "foo" and "Foo" are considered
/// equivalent.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct HTTPHeaderMatch {
    #[serde(default, skip_serializing_if = "is_default")]
    pub r#type: StringMatchType,
    pub name: HTTPHeaderName,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq, Default)]
pub enum StringMatchType {
    #[default]
    Exact,
    RegularExpression,
}

/// HTTPQueryParamMatch describes how to select a HTTP route by matching HTTP
/// query parameters.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct HTTPQueryParamMatch {
    #[serde(default, skip_serializing_if = "is_default")]
    pub r#type: StringMatchType,
    pub name: HTTPHeaderName,
    pub value: String,
}

/// HTTPMethod describes how to select a HTTP route by matching the HTTP
/// method as defined by
/// [RFC 7231](https://datatracker.ietf.org/doc/html/rfc7231#section-4) and
/// [RFC 5789](https://datatracker.ietf.org/doc/html/rfc5789#section-2).
/// The value is expected in upper case.
pub type HTTPMethod = String;

/// HTTPRouteFilter defines processing steps that must be completed during the
/// request or response lifecycle. HTTPRouteFilters are meant as an extension
/// point to express processing that may be done in Gateway implementations. Some
/// examples include request or response modification, implementing
/// authentication strategies, rate-limiting, and traffic shaping. API
/// guarantee/conformance is defined based on the type of the filter.
///
/// If a reference to a custom filter type cannot be resolved, the filter
/// MUST NOT be skipped. Instead, requests that would have been processed by
/// that filter MUST receive a HTTP error response.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum HTTPRouteFilter {
    /// RequestHeaderModifier defines a schema for a filter that modifies request
    /// headers.
    #[serde(rename_all = "camelCase")]
    RequestHeaderModifier {
        request_header_modifier: HTTPRequestHeaderFilter,
    },

    /// ResponseHeaderModifier defines a schema for a filter that modifies
    /// response headers.
    #[serde(rename_all = "camelCase")]
    ResponseHeaderModifier {
        response_header_modifier: HTTPRequestHeaderFilter,
    },

    /// RequestMirror defines a schema for a filter that mirrors requests.
    /// Requests are sent to the specified destination, but responses from
    /// that destination are ignored.
    ///
    /// This filter can be used multiple times within the same rule. Note that
    /// not all implementations will be able to support mirroring to multiple
    /// backends.
    #[serde(rename_all = "camelCase")]
    RequestMirror {
        request_mirror: HTTPRequestMirrorFilter,
    },

    /// RequestRedirect defines a schema for a filter that responds to the
    /// request with an HTTP redirection.
    #[serde(rename_all = "camelCase")]
    RequestRedirect {
        request_redirect: HTTPRequestRedirectFilter,
    },

    /// URLRewrite defines a schema for a filter that modifies a request during forwarding.
    #[serde(rename_all = "camelCase")]
    URLRewrite { url_rewrite: HTTPUrlRewriteFilter },

    /// ExtensionRef is an optional, implementation-specific extension to the
    /// "filter" behavior.  For example, resource "myroutefilter" in group
    /// "networking.example.net").
    ///
    /// This filter can be used multiple times within the same rule.
    #[serde(rename_all = "camelCase")]
    ExtensionRef { extension_ref: LocalObjectReference },
}

/// HTTPRequestHeaderFilter defines configuration for the RequestHeaderModifier
/// filter.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct HTTPRequestHeaderFilter {
    /// Set overwrites the request with the given header (name, value)
    /// before the action.
    ///
    /// Input:
    ///   GET /foo HTTP/1.1
    ///   my-header: foo
    ///
    /// Config:
    ///   set:
    ///   - name: "my-header"
    ///     value: "bar"
    ///
    /// Output:
    ///   GET /foo HTTP/1.1
    ///   my-header: bar
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<HTTPHeader>,

    /// Add adds the given header(s) (name, value) to the request
    /// before the action. It appends to any existing values associated
    /// with the header name.
    ///
    /// Input:
    ///   GET /foo HTTP/1.1
    ///   my-header: foo
    ///
    /// Config:
    ///   add:
    ///   - name: "my-header"
    ///     value: "bar"
    ///
    /// Output:
    ///   GET /foo HTTP/1.1
    ///   my-header: foo
    ///   my-header: bar
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<HTTPHeader>,

    /// Remove the given header(s) from the HTTP request before the action. The
    /// value of Remove is a list of HTTP header names. Note that the header
    /// names are case-insensitive (see
    /// <https://datatracker.ietf.org/doc/html/rfc2616#section-4.2>).
    ///
    /// Input:
    ///   GET /foo HTTP/1.1
    ///   my-header1: foo
    ///   my-header2: bar
    ///   my-header3: baz
    ///
    /// Config:
    ///   remove: ["my-header1", "my-header3"]
    ///
    /// Output:
    ///   GET /foo HTTP/1.1
    ///   my-header2: bar
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

/// HTTPHeader represents an HTTP Header name and value as defined by RFC 7230.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct HTTPHeader {
    /// Name is the name of the HTTP Header to be matched. Name matching MUST be
    /// case insensitive. (See <https://tools.ietf.org/html/rfc7230#section-3.2>).
    ///
    /// If multiple entries specify equivalent header names, the first entry with
    /// an equivalent name MUST be considered for a match. Subsequent entries
    /// with an equivalent header name MUST be ignored. Due to the
    /// case-insensitivity of header names, "foo" and "Foo" are considered
    /// equivalent.
    pub name: HTTPHeaderName,

    /// Value is the value of HTTP Header to be matched.
    pub value: String,
}

/// HTTPPathModifier defines configuration for path modifiers.
///
// gateway:experimental
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum HTTPPathModifier {
    /// ReplaceFullPath specifies the value with which to replace the full path
    /// of a request during a rewrite or redirect.
    #[serde(rename_all = "camelCase")]
    ReplaceFullPath { replace_full_path: String },

    /// ReplacePrefixMatch specifies the value with which to replace the prefix
    /// match of a request during a rewrite or redirect. For example, a request
    /// to "/foo/bar" with a prefix match of "/foo" and a ReplacePrefixMatch
    /// of "/xyz" would be modified to "/xyz/bar".
    ///
    /// Note that this matches the behavior of the PathPrefix match type. This
    /// matches full path elements. A path element refers to the list of labels
    /// in the path split by the `/` separator. When specified, a trailing `/` is
    /// ignored. For example, the paths `/abc`, `/abc/`, and `/abc/def` would all
    /// match the prefix `/abc`, but the path `/abcd` would not.
    ///
    /// ReplacePrefixMatch is only compatible with a `PathPrefix` HTTPRouteMatch.
    /// Using any other HTTPRouteMatch type on the same HTTPRouteRule will result in
    /// the implementation setting the Accepted Condition for the Route to `status: False`.
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

/// HTTPRequestRedirect defines a filter that redirects a request. This filter
/// MUST not be used on the same Route rule as a HTTPURLRewrite filter.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct HTTPRequestRedirectFilter {
    /// Scheme is the scheme to be used in the value of the `Location`
    /// header in the response.
    /// When empty, the scheme of the request is used.
    ///
    /// Scheme redirects can affect the port of the redirect, for more information,
    /// refer to the documentation for the port field of this filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,

    /// Hostname is the hostname to be used in the value of the `Location`
    /// header in the response.
    /// When empty, the hostname in the `Host` header of the request is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<PreciseHostname>,

    /// Path defines parameters used to modify the path of the incoming request.
    /// The modified path is then used to construct the `Location` header. When
    /// empty, the request path is used as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<HTTPPathModifier>,

    /// Port is the port to be used in the value of the `Location`
    /// header in the response.
    ///
    /// If no port is specified, the redirect port MUST be derived using the
    /// following rules:
    ///
    /// * If redirect scheme is not-empty, the redirect port MUST be the well-known
    ///   port associated with the redirect scheme. Specifically "http" to port 80
    ///   and "https" to port 443. If the redirect scheme does not have a
    ///   well-known port, the listener port of the Gateway SHOULD be used.
    /// * If redirect scheme is empty, the redirect port MUST be the Gateway
    ///   Listener port.
    ///
    /// Implementations SHOULD NOT add the port number in the 'Location'
    /// header in the following cases:
    ///
    /// * A Location header that will use HTTP (whether that is determined via
    ///   the Listener protocol or the Scheme field) _and_ use port 80.
    /// * A Location header that will use HTTPS (whether that is determined via
    ///   the Listener protocol or the Scheme field) _and_ use port 443.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,

    /// StatusCode is the HTTP status code to be used in response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

/// HTTPURLRewriteFilter defines a filter that modifies a request during
/// forwarding. At most one of these filters may be used on a Route rule. This
/// may not be used on the same Route rule as a HTTPRequestRedirect filter.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct HTTPUrlRewriteFilter {
    /// Hostname is the value to be used to replace the Host header value during
    /// forwarding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<PreciseHostname>,

    /// Path defines a path rewrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<HTTPPathModifier>,
}

/// HTTPRequestMirrorFilter defines configuration for the RequestMirror filter.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]

pub struct HTTPRequestMirrorFilter {
    /// Percent represents the percentage of requests that should be mirrored to BackendRef.
    /// Its minimum value is 0 (indicating 0% of requests) and its maximum value is 100 (indicating 100% of requests).
    ///
    /// Only one of Fraction or Percent may be specified. If neither field is specified, 100% of requests will be mirrored.
    pub percent: Option<i32>,

    /// Percent represents the percentage of requests that should be mirrored to BackendRef.
    /// Its minimum value is 0 (indicating 0% of requests) and its maximum value is 100 (indicating 100% of requests).
    ///
    /// Only one of Fraction or Percent may be specified. If neither field is specified, 100% of requests will be mirrored.
    pub fraction: Option<(i32, i32)>,

    /// BackendRef references a resource where mirrored requests are sent.
    ///
    /// Mirrored requests must be sent only to a single destination endpoint
    /// within this BackendRef, irrespective of how many endpoints are present
    /// within this BackendRef.
    ///
    /// If the referent cannot be found, this BackendRef is invalid and must be
    /// dropped from the Gateway. The controller must ensure the "ResolvedRefs"
    /// condition on the Route status is set to `status: False` and not configure
    /// this backend in the underlying implementation.
    ///
    /// If there is a cross-namespace reference to an *existing* object
    /// that is not allowed by a ReferenceGrant, the controller must ensure the
    /// "ResolvedRefs"  condition on the Route is set to `status: False`,
    /// with the "RefNotPermitted" reason and not configure this backend in the
    /// underlying implementation.
    ///
    /// In either error case, the Message of the `ResolvedRefs` Condition
    /// should be used to provide more detail about the problem.
    pub backend_ref: BackendObjectReference,
}

/// HTTPBackendRef defines how a HTTPRoute forwards a HTTP request.
///
/// Note that when a namespace different than the local namespace is specified, a
/// ReferenceGrant object is required in the referent namespace to allow that
/// namespace's owner to accept the reference. See the ReferenceGrant
/// documentation for details.
///
/// When the BackendRef points to a Kubernetes Service, implementations SHOULD
/// honor the appProtocol field if it is set for the target Service Port.
///
/// Implementations supporting appProtocol SHOULD recognize the Kubernetes
/// Standard Application Protocols defined in KEP-3726.
///
/// If a Service appProtocol isn't specified, an implementation MAY infer the
/// backend protocol through its own means. Implementations MAY infer the
/// protocol from the Route type referring to the backend Service.
///
/// If a Route is not able to send traffic to the backend using the specified
/// protocol then the backend is considered invalid. Implementations MUST set the
/// "ResolvedRefs" condition to "False" with the "UnsupportedProtocol" reason.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HTTPBackendRef {
    /// BackendRef is a reference to a backend to forward matched requests to.
    ///
    /// A BackendRef can be invalid for the following reasons. In all cases, the
    /// implementation MUST ensure the `ResolvedRefs` Condition on the Route
    /// is set to `status: False`, with a Reason and Message that indicate
    /// what is the cause of the error.
    ///
    /// A BackendRef is invalid if:
    ///
    /// * It refers to an unknown or unsupported kind of resource. In this
    ///   case, the Reason must be set to `InvalidKind` and Message of the
    ///   Condition must explain which kind of resource is unknown or unsupported.
    ///
    /// * It refers to a resource that does not exist. In this case, the Reason must
    ///   be set to `BackendNotFound` and the Message of the Condition must explain
    ///   which resource does not exist.
    ///
    /// * It refers a resource in another namespace when the reference has not been
    ///   explicitly allowed by a ReferenceGrant (or equivalent concept). In this
    ///   case, the Reason must be set to `RefNotPermitted` and the Message of the
    ///   Condition must explain which cross-namespace reference is not allowed.
    ///
    /// * It refers to a Kubernetes Service that has an incompatible appProtocol
    ///   for the given Route type
    ///
    /// * The BackendTLSPolicy object is installed in the cluster, a BackendTLSPolicy
    ///   is present that refers to the Service, and the implementation is unable
    ///   to meet the requirement. At the time of writing, BackendTLSPolicy is
    ///   experimental, but once it becomes standard, this will become a MUST
    ///   requirement.
    #[serde(flatten)]
    pub backend_ref: Option<BackendRef>,

    /// Filters defined at this level should be executed if and only if the
    /// request is being forwarded to the backend defined here.
    ///
    /// Support: Implementation-specific (For broader support of filters, use the
    /// Filters field in HTTPRouteRule.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<HTTPRouteFilter>,
}

///
/// Now get into xDS deserialization for the above
///
use xds_api::pb::envoy::{
    config::route::v3::{self as xds_route, query_parameter_matcher::QueryParameterMatchSpecifier},
    r#type::matcher::v3::string_matcher::MatchPattern,
};

impl HTTPRouteTimeouts {
    pub fn from_xds(r: &xds_route::RouteAction) -> Option<Self> {
        let request = r.timeout.clone().map(|x| x.try_into().unwrap());
        let mut backend_request: Option<Duration> = None;
        if let Some(r1) = &r.retry_policy {
            backend_request = r1.per_try_timeout.clone().map(|x| x.try_into().unwrap());
        }
        if request.is_some() || backend_request.is_some() {
            return Some(HTTPRouteTimeouts {
                backend_request: backend_request,
                request: request,
            });
        } else {
            return None;
        }
    }
}

impl HTTPRouteMatch {
    pub fn from_xds(r: &xds_route::RouteMatch) -> Option<Self> {
        let path = r.path_specifier.as_ref().and_then(HTTPPathMatch::from_xds);

        // with xds, any method match is converted into a match on a ":method" header. it does not have a way
        // of specifying a method match otherwise. to keep the "before and after" xDS as similar as possible then
        // we pull this out of the headers list if it exists
        let mut method: Option<HTTPMethod> = None;
        let headers = r
            .headers
            .iter()
            .filter_map(HTTPHeaderMatch::from_xds)
            .filter({
                |header| {
                    if header.name == ":method" {
                        match header.r#type {
                            StringMatchType::Exact => method = Some(header.value.clone()),
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
            .filter_map(HTTPQueryParamMatch::from_xds)
            .collect();

        Some(HTTPRouteMatch {
            headers,
            method,
            path,
            query_params,
        })
    }
}

impl HTTPQueryParamMatch {
    pub fn from_xds(matcher: &xds_route::QueryParameterMatcher) -> Option<Self> {
        let (r#type, value) = match matcher.query_parameter_match_specifier.as_ref()? {
            QueryParameterMatchSpecifier::StringMatch(s) => match s.match_pattern.as_ref() {
                Some(MatchPattern::Exact(s)) => {
                    //FIXME: if case insensitive is set, should convert to a regex
                    //or maybe just throw an error
                    (StringMatchType::Exact, s.clone())
                }
                Some(MatchPattern::SafeRegex(s)) => {
                    //fixme: raise error if wrong type of regular expression
                    (StringMatchType::RegularExpression, s.regex.clone())
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
        Some(HTTPQueryParamMatch {
            name,
            r#type: r#type,
            value,
        })
    }
}

impl HTTPHeaderMatch {
    fn from_xds(header_matcher: &xds_route::HeaderMatcher) -> Option<Self> {
        let name = header_matcher.name.clone();
        let (r#type, value) = match header_matcher.header_match_specifier.as_ref()? {
            xds_route::header_matcher::HeaderMatchSpecifier::ExactMatch(s) => {
                (StringMatchType::Exact, s.clone())
            }
            xds_route::header_matcher::HeaderMatchSpecifier::SafeRegexMatch(pfx) => {
                //fixme: we should validate engine type and somewhow record if we dont understand it
                (StringMatchType::RegularExpression, pfx.regex.clone())
            }
            _ => {
                // FIXME: log/record that we are throwing away config
                return None;
            }
        };

        Some(HTTPHeaderMatch {
            name,
            r#type: r#type,
            value,
        })
    }
}

impl HTTPPathMatch {
    fn from_xds(path_spec: &xds_route::route_match::PathSpecifier) -> Option<Self> {
        return match path_spec {
            xds_route::route_match::PathSpecifier::Prefix(p) => Some(HTTPPathMatch {
                r#type: HTTPPathMatchType::PathPrefix,
                value: p.clone(),
            }),
            xds_route::route_match::PathSpecifier::Path(p) => Some(HTTPPathMatch {
                r#type: HTTPPathMatchType::Exact,
                value: p.clone(),
            }),
            xds_route::route_match::PathSpecifier::SafeRegex(p) => Some(HTTPPathMatch {
                r#type: HTTPPathMatchType::RegularExpression,
                value: p.regex.clone(),
            }),
            _ => {
                // FIXME: log/record that we are throwing away config
                None
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use crate::gateway_api::httproute::HTTPRouteRule;

    #[test]
    fn parses_http_route() {
        let test_json = serde_json::from_str::<serde_json::Value>(
            r#"{
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
            "backendRefs":[
                { "weight": 1, "kind":"Service", "name": "timeout-svc", "port": 8080 }
            ],
            "timeouts": {
                "request": "1s"
            }
        }"#,
        )
        .unwrap();
        let obj: HTTPRouteRule = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
