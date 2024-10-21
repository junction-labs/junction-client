"""Junction client configuration."""

# This file is automatically generated with junction-api-gen. Do not edit!
import typing


Duration = str | int | float
"""A duration expressed as a number of seconds or a string like '1h30m27s42ms'"""


class TargetDNS(typing.TypedDict):
    type: typing.Literal["DNS"]
    hostname: str
    """The DNS name to target."""

    port: int
    """The port number to target/attach to.

    When attaching policies, if it is not specified, the target will apply
    to all connections that don't have a specific port specified.

    When being used to lookup a backend after a matched rule, if it is not
    specified then it will use the same port as the incoming request"""


class TargetService(typing.TypedDict):
    type: typing.Literal["Service"]
    name: str
    """The name of the Kubernetes Service to target."""

    namespace: str
    """The namespace of the Kubernetes service to target. This must be explicitly
    specified, and won't be inferred from context."""

    port: int
    """The port number of the Kubernetes service to target/ attach to.

    When attaching policies, if it is not specified, the target will apply
    to all connections that don't have a specific port specified.

    When being used to lookup a backend after a matched rule, if it is not
    specified then it will use the same port as the incoming request"""


Target = TargetDNS | TargetService


class Fraction(typing.TypedDict):
    """A fraction, expressed as a numerator and a denominator."""

    numerator: int
    denominator: int


class WeightedTarget(typing.TypedDict):
    weight: int
    hostname: str
    """The DNS name to target."""

    name: str
    """The name of the Kubernetes Service to target."""

    namespace: str
    """The namespace of the Kubernetes service to target. This must be explicitly
    specified, and won't be inferred from context."""

    port: int
    """The port number to target/attach to.

    When attaching policies, if it is not specified, the target will apply
    to all connections that don't have a specific port specified.

    When being used to lookup a backend after a matched rule, if it is not
    specified then it will use the same port as the incoming request"""

    type: typing.Literal["DNS"] | typing.Literal["Service"]


class RouteTimeouts(typing.TypedDict):
    """Defines timeouts that can be configured for a HTTP Route."""

    request: Duration
    """Specifies the maximum duration for a HTTP request. This timeout is intended to cover as
    close to the whole request-response transaction as possible.

    An entire client HTTP transaction may result in more than one call to destination backends,
    for example, if automatic retries are supported.

    Specifying a zero value such as "0s" is interpreted as no timeout."""

    backend_request: Duration
    """Specifies a timeout for an individual request to a backend. This covers the time from when
    the request first starts being sent to when the full response has been received from the
    backend.

    An entire client HTTP transaction may result in more than one call to the destination
    backend, for example, if retries are configured.

    Because the Request timeout encompasses the BackendRequest timeout, the value of
    BackendRequest must be <= the value of Request timeout.

    Specifying a zero value such as "0s" is interpreted as no timeout."""


class RouteRetry(typing.TypedDict):
    """Specifies a way of configuring client retry policy.

    Modelled on the forthcoming [Gateway API type](https://gateway-api.sigs.k8s.io/geps/gep-1731/)."""

    codes: typing.List[int]
    attempts: int
    backoff: Duration


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
    """Defines the predicate used to match requests to a given action. Multiple match types are ANDed
    together, i.e. the match will evaluate to true only if all conditions are satisfied.

    For example, the match below will match a HTTP request only if its path starts with `/foo` AND
    it contains the `version: v1` header::

     ```yaml
     match:
       path:
         value: "/foo"
       headers:
       - name: "version"
         value "v1"
     ```"""

    path: PathMatch
    """Specifies a HTTP request path matcher. If this field is not specified, a default prefix
    match on the "/" path is provided."""

    headers: typing.List[HeaderMatch]
    """Specifies HTTP request header matchers. Multiple match values are ANDed together, meaning, a
    request must match all the specified headers to select the route."""

    query_params: typing.List[QueryParamMatch]
    """Specifies HTTP query parameter matchers. Multiple match values are ANDed together, meaning,
    a request must match all the specified query parameters to select the route."""

    method: str
    """Specifies HTTP method matcher. When specified, this route will be matched only if the
    request has the specified method."""


class RouteRule(typing.TypedDict):
    """Defines semantics for matching an HTTP request based on conditions (matches), processing it
    (filters), and forwarding the request to an API object (backendRefs)."""

    matches: typing.List[RouteMatch]
    """Defines conditions used for matching the rule against incoming HTTP requests. Each match is
    independent, i.e. this rule will be matched if **any** one of the matches is satisfied.

    For example, take the following matches configuration::

     ```yaml
     matches:
     - path:
         value: "/foo"
       headers:
       - name: "version"
         value: "v2"
     - path:
         value: "/v2/foo"
     ```

    For a request to match against this rule, a request must satisfy EITHER of the two
    conditions:

    - path prefixed with `/foo` AND contains the header `version: v2`
    - path prefix of `/v2/foo`

    See the documentation for RouteMatch on how to specify multiple match conditions that should
    be ANDed together.

    If no matches are specified, the default is a prefix path match on "/", which has the effect
    of matching every HTTP request."""

    timeouts: RouteTimeouts
    retry: RouteRetry
    """How to retry any requests to this route."""

    backends: typing.List[WeightedTarget]
    """Where the traffic should route if this rule matches."""


class Route(typing.TypedDict):
    """High level policy that describes how a request to a specific hostname should
    be routed.

    Routes contain a target that describes the hostname to match and at least
    one [RouteRule]. When a [RouteRule] matches, it also describes where and how
    the traffic should be directed to a [Backend](crate::backend::Backend)."""

    target: Target
    """The target for this route. The target determines the hostnames that map
    to this route."""

    rules: typing.List[RouteRule]
    """The route rules that determine whether any URLs match."""


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

    A backend configures how all traffic for it's `target` is handled. Any
    traffic routed to this backend will use its load balancing policy to evenly
    spread traffic across all available endpoints."""

    target: Target
    """The target this backend represents. A target may be a Kubernetes Service
    or a DNS name. See [Target] for more."""

    lb: LbPolicy
    """How traffic to this target should be load balanced."""
