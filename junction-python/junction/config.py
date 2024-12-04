"""Junction client configuration."""

# This file is automatically generated with junction-api-gen. Do not edit!
import typing


Duration = str | int | float
"""A duration expressed as a total number of seconds. Durations should never be negative."""


class ServiceDns(typing.TypedDict):
    type: typing.Literal["Dns"]
    hostname: str
    """A valid RFC1123 DNS domain name."""


class ServiceKube(typing.TypedDict):
    type: typing.Literal["Kube"]
    name: str
    """The name of the Kubernetes Service to target."""

    namespace: str
    """The namespace of the Kubernetes service to target. This must be explicitly
    specified, and won't be inferred from context."""


Service = ServiceDns | ServiceKube


class Fraction(typing.TypedDict):
    """A fraction, expressed as a numerator and a denominator."""

    numerator: int
    denominator: int


class BackendRef(typing.TypedDict):
    hostname: str
    """A valid RFC1123 DNS domain name."""

    name: str
    """The name of the Kubernetes Service to target."""

    namespace: str
    """The namespace of the Kubernetes service to target. This must be explicitly
    specified, and won't be inferred from context."""

    type: typing.Literal["Dns"] | typing.Literal["Kube"]
    port: int
    """The port to route traffic to, used in combination with
    [service][Self::service] to identify the
    [Backend][crate::backend::Backend] to route traffic to.

    If omitted, the port of the incoming request is used to route traffic."""

    weight: int
    """The relative weight of this backend relative to any other backends in
    [the list][RouteRule::backends].

    If not specified, defaults to `1`.

    An individual backend may have a weight of `0`, but specifying every
    backend with `0` weight is an error."""


class RouteTimeouts(typing.TypedDict):
    """Defines timeouts that can be configured for a HTTP Route."""

    request: Duration
    """Specifies the maximum duration for a HTTP request. This timeout is
    intended to cover as close to the whole request-response transaction as
    possible.

    An entire client HTTP transaction may result in more than one call to
    destination backends, for example, if automatic retries are configured."""

    backend_request: Duration
    """Specifies a timeout for an individual request to a backend. This covers
    the time from when the request first starts being sent to when the full
    response has been received from the backend.

    Because the overall request timeout encompasses the backend request
    timeout, the value of this timeout must be less than or equal to the
    value of the overall timeout."""


class RouteRetry(typing.TypedDict):
    """Configure client retry policy."""

    codes: typing.List[int]
    """The HTTP error codes that retries should be applied to."""

    attempts: int
    """The total number of attempts to make when retrying this request."""

    backoff: Duration
    """The amount of time to back off between requests during a series of
    retries."""


class HeaderValue(typing.TypedDict):
    name: str
    """The name of the HTTP Header. Note header names are case insensitive. (See
    <https://tools.ietf.org/html/rfc7230#section-3.2>)."""

    value: str
    """The value of HTTP Header."""


class HeaderMatchRegularExpression(typing.TypedDict):
    type: typing.Literal["RegularExpression"]
    name: str
    value: str


class HeaderMatchExact(typing.TypedDict):
    type: typing.Literal["Exact"]
    name: str
    value: str


HeaderMatch = HeaderMatchRegularExpression | HeaderMatchExact


class QueryParamMatchRegularExpression(typing.TypedDict):
    type: typing.Literal["RegularExpression"]
    name: str
    value: str


class QueryParamMatchExact(typing.TypedDict):
    type: typing.Literal["Exact"]
    name: str
    value: str


QueryParamMatch = QueryParamMatchRegularExpression | QueryParamMatchExact


class PathMatchPrefix(typing.TypedDict):
    type: typing.Literal["Prefix"]
    value: str


class PathMatchRegularExpression(typing.TypedDict):
    type: typing.Literal["RegularExpression"]
    value: str


class PathMatchExact(typing.TypedDict):
    type: typing.Literal["Exact"]
    value: str


PathMatch = PathMatchPrefix | PathMatchRegularExpression | PathMatchExact


class RouteMatch(typing.TypedDict):
    """Defines the predicate used to match requests to a given action. Multiple
    match types are ANDed together; the match will evaluate to true only if all
    conditions are satisfied. For example, if a match specifies a `path` match
    and two `query_params` matches, it will match only if the request's path
    matches and both of the `query_params` are matches.

    The default RouteMatch functions like a path match on the empty prefix,
    which matches every request."""

    path: PathMatch
    """Specifies a HTTP request path matcher."""

    headers: typing.List[HeaderMatch]
    """Specifies HTTP request header matchers. Multiple match values are ANDed
    together, meaning, a request must match all the specified headers."""

    query_params: typing.List[QueryParamMatch]
    """Specifies HTTP query parameter matchers. Multiple match values are ANDed
    together, meaning, a request must match all the specified query
    parameters."""

    method: str
    """Specifies HTTP method matcher. When specified, this route will be
    matched only if the request has the specified method."""


class RouteRule(typing.TypedDict):
    """A RouteRule contains a set of matches that define which requests it applies
    to, processing rules, and the final destination(s) for matching traffic.

    See the Junction docs for a high level description of how Routes and
    RouteRules behave."""

    matches: typing.List[RouteMatch]
    """A list of match rules applied to an outgoing request.  Each match is
    independent; this rule will be matched if **any** of the listed matches
    is satsified.

    If no matches are specified, this Rule matches any outgoing request."""

    timeouts: RouteTimeouts
    retry: RouteRetry
    """How to retry requests. If not specified, requests are not retried."""

    backends: typing.List[BackendRef]
    """Where the traffic should route if this rule matches.

    If no backends are specified, this route becomes a black hole for
    traffic and all matching requests return an error."""


class Route(typing.TypedDict):
    """A Route is a policy that describes how a request to a specific virtual
    host should be routed."""

    id: str
    """A globally unique identifier for this Route.

    Route IDs must be valid RFC 1035 DNS label names - they must start with
    a lowercase ascii character, and can only contain lowercase ascii
    alphanumeric characters and the `-` character."""

    tags: typing.Dict[str, str]
    """A list of arbitrary tags that can be added to a Route."""

    hostnames: typing.List[str]
    """The hostnames that match this Route."""

    ports: typing.List[int]
    """The ports that match this Route."""

    rules: typing.List[RouteRule]
    """The rules that determine whether a request matches and where traffic
    should be routed."""


class BackendId(typing.TypedDict):
    """A Backend is uniquely identifiable by a combination of Service and port.

    [Backend][crate::backend::Backend]."""

    hostname: str
    """A valid RFC1123 DNS domain name."""

    name: str
    """The name of the Kubernetes Service to target."""

    namespace: str
    """The namespace of the Kubernetes service to target. This must be explicitly
    specified, and won't be inferred from context."""

    type: typing.Literal["Dns"] | typing.Literal["Kube"]
    port: int
    """The port backend traffic is sent on."""


class SessionAffinityHashParam(typing.TypedDict):
    terminal: bool
    """Whether to stop immediately after hashing this value.

    This is useful if you want to try to hash a value, and then fall back to another as a
    default if it wasn't set."""

    name: str
    """The name of the header to use as hash input."""

    type: typing.Literal["Header"]


class SessionAffinity(typing.TypedDict):
    hash_params: typing.List[SessionAffinityHashParam]


class LbPolicyRoundRobin(typing.TypedDict):
    type: typing.Literal["RoundRobin"]


class LbPolicyRingHash(typing.TypedDict):
    type: typing.Literal["RingHash"]
    min_ring_size: int
    """The minimum size of the hash ring"""

    hash_params: typing.List[SessionAffinityHashParam]
    """How to hash an outgoing request into the ring.

    Hash parameters are applied in order. If the request is missing an input, it has no effect
    on the final hash. Hashing stops when only when all polices have been applied or a
    `terminal` policy matches part of an incoming request.

    This allows configuring a fallback-style hash, where the value of `HeaderA` gets used,
    falling back to the value of `HeaderB`.

    If no policies match, a random hash is generated for each request."""


class LbPolicyUnspecified(typing.TypedDict):
    type: typing.Literal["Unspecified"]


LbPolicy = LbPolicyRoundRobin | LbPolicyRingHash | LbPolicyUnspecified


class Backend(typing.TypedDict):
    """A Backend is a logical target for network traffic.

    A backend configures how all traffic for its `target` is handled. Any
    traffic routed to this backend will use the configured load balancing policy
    to spread traffic across available endpoints."""

    id: BackendId
    """A unique identifier for this backend."""

    lb: LbPolicy
    """How traffic to this target should be load balanced."""
