//! HTTP [Route] configuration. [Route]s dynamically congfigure things you might
//! put directly in client code like timeouts and retries, failure detection, or
//! picking a different backend based on request data.
//!
//! # Routes and URLs
//!
//! [Route]s are uniquely identified by a [VirtualHost], which is the
//! combination of a [Target] and an optional port. Every [VirtualHost] has a
//! DNS hostname and port. When making an HTTP request a Route is selected by
//! using the hostname and port the request URL as the [name of a
//! VirtualHost][VirtualHost::name] and finding a matching route.
//!
//! Once a [Route] has been selected, each [rule][RouteRule] is applied to an
//! entire request to find an appropriate match. See the next section for a
//! high-level description of matching, and the documentation of
//! [matches][RouteRule::matches] and [RouteMatch] for more detail.
//!
//! # Route Rules
//!
//! A [Route] contains zero or more [rules][RouteRule] that describe how to
//! match a request. Each rule is made of up a set of
//! [matches][RouteRule::matches], a set of [backends][RouteRule::backends] to
//! send the request to once a match has been found, and policy on how to handle
//! retries and failures. For example, the following `Route` uses a Kubernetes
//! Service as a VirtualHost, matches all traffic to it, and splits it evenly
//! between a DNS and a Service backend, while applying a simple retry policy:
//!
//! ```
//! # use junction_api::*;
//! # use junction_api::http::*;
//! // an example route
//! let route = Route {
//!     vhost: Target::kube_service("prod", "example-svc").unwrap().into_vhost(None),
//!     tags: Default::default(),
//!     rules: vec![
//!         RouteRule {
//!             retry: Some(RouteRetry {
//!                 codes: vec![500, 503],
//!                 attempts: Some(3),
//!                 backoff: Some(Duration::from_millis(500)),
//!             }),
//!             backends: vec![
//!                 WeightedBackend {
//!                     weight: 1,
//!                     backend: Target::dns("prod.old-thing.internal")
//!                         .unwrap()
//!                         .into_backend(1234),
//!                 },
//!                 WeightedBackend {
//!                     weight: 1,
//!                     backend: Target::kube_service("prod", "new-thing")
//!                         .unwrap()
//!                         .into_backend(8891),
//!                 },
//!             ],
//!             ..Default::default()
//!         }
//!     ],
//! };
//! ```
//!
//! See the [RouteRule] documentation for all of your configuration options,
//! and [RouteMatch] for different ways to match an incoming request.
//!
//! # Matching
//!
//! A `RouteRule`s is applied to an outgoing request if any of it's
//! [matches][RouteRule::matches] matches a request. For example, a rule with
//! the following matches:
//!
//! ```
//! # use junction_api::*;
//! # use junction_api::http::*;
//! let matches = vec![
//!     RouteMatch {
//!         path: Some(PathMatch::Exact { value: "/foo".to_string() }),
//!         headers: vec![
//!             HeaderMatch::Exact { name: "version".to_string(), value: "v2".to_string() },
//!         ],
//!         ..Default::default()
//!     },
//!     RouteMatch {
//!         path: Some(PathMatch::Prefix { value: "/v2/foo".to_string() }),
//!         ..Default::default()
//!     },
//! ];
//! ```
//!
//! would match a request with path `/v2/foo` OR a request with path `/foo` and
//! the `version: v2` header set. Any request to `/foo` without that header set
//! would not match.
//!
//! # Passthrough Routes
//!
//! A [RouteRule] with no backends is called a "passthrough rule". Instead of
//! dead-ending traffic, it uses the Route's [VirtualHost] as a traffic target,
//! convering it into a [BackendId] on the fly, using the port of the incoming
//! request if the [VirtualHost] has no port specified. (see
//! [VirtualHost::into_backend]).
//!
//! For example, given the following route will apply a retry policy to all
//! requests to `http://example.internal` and use the request port to find
//! a backend.
//!
//! ```
//! # use junction_api::*;
//! # use junction_api::http::*;
//! let route = Route {
//!     vhost: Target::dns("example.internal").unwrap().into_vhost(None),
//!     tags: Default::default(),
//!     rules: vec![
//!         RouteRule {
//!             retry: Some(RouteRetry {
//!                 codes: vec![500, 503],
//!                 attempts: Some(3),
//!                 ..Default::default()
//!             }),
//!             ..Default::default()
//!         }
//!     ],
//! };
//! ```
//!
//! A request to `http://example.internal:8801` would get sent to a
//! [backend][BackendId] with the port `8801` and a request to
//! `http://example.internal:443` would get sent to a backend with port `:443`.
//!
//! A route with no rules is a more general case of a passthrough route; it's
//! equivalent to specifying a single [RouteRule] with no backends that always
//! matches.

use std::collections::BTreeMap;

use crate::{
    shared::{Duration, Fraction, Regex},
    BackendId, Hostname, Name, Target, VirtualHost,
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

#[doc(hidden)]
pub mod tags {
    //! Well known tags for Routes.

    /// Marks a Route as generated by an automated process and NOT authored by a
    /// human being. Any Route with this tag will have lower priority than a
    /// Route authored by a human.
    ///
    /// The value of this tag should be a process or application ID that
    /// indicates where the route came from and what generated it.
    pub const GENERATED_BY: &str = "junctionlabs.io/generated-by";
}

/// A Route is a policy that describes how a request to a specific virtual
/// host should be routed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Route {
    /// A virtual hostname that uniquely identifies this route.
    pub vhost: VirtualHost,

    /// A list of arbitrary tags that can be added to a Route.
    #[serde(default)]
    // TODO: limit this a-la kube annotation keys/values.
    pub tags: BTreeMap<String, String>,

    /// The rules that determine whether a request matches and where traffic
    /// should be routed.
    #[serde(default)]
    pub rules: Vec<RouteRule>,
}

impl Route {
    /// Create a trivial route that passes all traffic for a target directly to
    /// the same backend target.
    ///
    /// If this RouteTarget has no port specified, `80` will be used for the
    /// backend.
    pub fn passthrough_route(target: VirtualHost) -> Route {
        Route {
            vhost: target,
            tags: Default::default(),
            rules: vec![RouteRule {
                matches: vec![RouteMatch {
                    path: Some(PathMatch::empty_prefix()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }
}

/// A RouteRule contains a set of matches that define which requests it applies
/// to, processing rules, and the final destination(s) for matching traffic.
///
/// See the Junction docs for a high level description of how Routes and
/// RouteRules behave.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteRule {
    /// A list of match rules applied to an outgoing request.  Each match is
    /// independent; this rule will be matched if **any** of the listed matches
    /// is satsified.
    ///
    /// If no matches are specified, this Rule matches any outgoing request.
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
    #[doc(hidden)]
    pub filters: Vec<RouteFilter>,

    // FIXME(persistence): enable session persistence as per the Gateway API #[serde(default,
    // skip_serializing_if = "Option::is_none")]
    // pub session_persistence: Option<SessionPersistence>,

    // The timeouts set on any request that matches route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<RouteTimeouts>,

    /// How to retry requests. If not specified, requests are not retried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RouteRetry>,

    /// Where the traffic should route if this rule matches.
    ///
    /// If no backends are specified, traffic is sent to the VirtualHost this
    /// route was defined with, using the request's port to fill in any
    /// defaults.
    #[serde(default)]
    pub backends: Vec<WeightedBackend>,
}

/// Defines timeouts that can be configured for a HTTP Route.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteTimeouts {
    /// Specifies the maximum duration for a HTTP request. This timeout is
    /// intended to cover as close to the whole request-response transaction as
    /// possible.
    ///
    /// An entire client HTTP transaction may result in more than one call to
    /// destination backends, for example, if automatic retries are configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Duration>,

    /// Specifies a timeout for an individual request to a backend. This covers
    /// the time from when the request first starts being sent to when the full
    /// response has been received from the backend.
    ///
    /// Because the overall request timeout encompasses the backend request
    /// timeout, the value of this timeout must be less than or equal to the
    /// value of the overall timeout.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "backendRequest"
    )]
    pub backend_request: Option<Duration>,
}

/// Defines the predicate used to match requests to a given action. Multiple
/// match types are ANDed together; the match will evaluate to true only if all
/// conditions are satisfied. For example, if a match specifies a `path` match
/// and two `query_params` matches, it will match only if the request's path
/// matches and both of the `query_params` are matches.
///
/// The default RouteMatch functions like a path match on the empty prefix,
/// which matches every request.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteMatch {
    /// Specifies a HTTP request path matcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathMatch>,

    /// Specifies HTTP request header matchers. Multiple match values are ANDed
    /// together, meaning, a request must match all the specified headers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderMatch>,

    /// Specifies HTTP query parameter matchers. Multiple match values are ANDed
    /// together, meaning, a request must match all the specified query
    /// parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "queryParams")]
    pub query_params: Vec<QueryParamMatch>,

    /// Specifies HTTP method matcher. When specified, this route will be
    /// matched only if the request has the specified method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Method>,
}

/// Describes how to select a HTTP route by matching the HTTP request path.  The
/// `type` of a match specifies how HTTP paths should be compared.
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
/// * ":method" - ":" is an invalid character. This means that HTTP/2 pseudo
///    headers are not currently supported by this type.
///
/// * "/invalid" - "/" is an invalid character
//
// FIXME: newtype and validate this. probably also make this Bytes or SmolString
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
//
// FIXME: actually do this only-the-first-entry matching thing
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
//
// FIXME: replace with http::Method
pub type Method = String;

/// Defines processing steps that must be completed during the request or
/// response lifecycle.
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
    pub hostname: Option<Name>,

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
    pub port: Option<u16>,

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
    pub hostname: Option<Hostname>,

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

/// Configure client retry policy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct RouteRetry {
    /// The HTTP error codes that retries should be applied to.
    //
    // TODO: should this be http::StatusCode?
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<u32>,

    /// The total number of attempts to make when retrying this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,

    /// The amount of time to back off between requests during a series of
    /// retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<Duration>,
}

const fn default_weight() -> u32 {
    1
}

/// The combination of a backend and a weight.
//
// TODO: gateway API also allows filters here under an extended support
// condition we need to decide whether this is one where its simpler just to
// drop it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct WeightedBackend {
    /// The relative weight of this backend relative to any other backends in
    /// [the list][RouteRule::backends].
    ///
    /// If not specified, defaults to `1`.
    ///
    /// An individual backend may have a weight of `0`, but specifying every
    /// backend with `0` weight is an error.
    #[serde(default = "default_weight")]
    pub weight: u32,

    /// The [Backend][crate::backend::Backend] to route to.
    #[serde(flatten)]
    pub backend: BackendId,
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
        Target,
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
    fn deserialize_retry_policy() {
        let test_json = json!({
            "codes":[ 1, 2 ],
            "attempts": 3,
            // NOTE: serde will happily read an int here, but Duration serializes as a float
            "backoff": 60.0,
        });
        let obj: RouteRetry = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(obj).unwrap();
        assert_eq!(test_json, output_json);
    }

    #[test]
    fn deserialize_route_rule() {
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
                { "weight": 1, "name": "timeout-svc", "namespace": "foo", "port": 80 }
            ],
            "timeouts": {
                "request": 1.0,
            }
        });
        let obj: RouteRule = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(&obj).unwrap();
        assert_eq!(test_json, output_json);
    }

    #[test]
    fn test_route_roundtrip() {
        assert_deserialize(
            json!({
                "vhost": { "name": "foo", "namespace": "bar" },
                "rules": [
                    {
                        "backends": [ { "name": "foo", "namespace": "bar", "port": 80 } ],
                    }
                ]
            }),
            Route {
                vhost: VirtualHost {
                    target: Target::kube_service("bar", "foo").unwrap(),
                    port: None,
                },
                tags: Default::default(),
                rules: vec![RouteRule {
                    matches: vec![],
                    filters: vec![],
                    timeouts: None,
                    retry: None,
                    backends: vec![WeightedBackend {
                        backend: BackendId {
                            target: Target::kube_service("bar", "foo").unwrap(),
                            port: 80,
                        },
                        weight: 1,
                    }],
                }],
            },
        );
    }

    #[test]
    fn test_route_missing_vhost() {
        assert_deserialize_err::<Route>(json!({
            "soemthing_else": ["foo.bar"],
            "rules": [
                {
                    "matches": [],
                }
            ]
        }));
    }

    #[track_caller]
    fn assert_deserialize<T: DeserializeOwned + PartialEq + std::fmt::Debug>(
        json: serde_json::Value,
        expected: T,
    ) {
        let actual: T = serde_json::from_value(json).unwrap();
        assert_eq!(expected, actual);
    }

    #[track_caller]
    fn assert_deserialize_err<T: DeserializeOwned + PartialEq + std::fmt::Debug>(
        json: serde_json::Value,
    ) -> serde_json::Error {
        serde_json::from_value::<T>(json).unwrap_err()
    }
}
