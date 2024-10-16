import typing

import urllib3
from urllib3 import BaseHTTPResponse
from urllib3.connectionpool import HTTPConnectionPool

import junction
import junction.junction as jct_core

# When integrating with urllib3, it makes the most sense to provide a drop-in
# replacement for PoolManager - HTTPConnectionPool and friends are pools of
# connections to indistinguishable endpoints, but to load balance effectively,
# we DO want to distinguish between endpoints.
#
# Practically, this means that we're writing a subclass of PoolManager that uses
# the connection_from_* methods to pick out a connection for the endpoints we'd
# like to send data to. urlopen is a nice place to do that, because it means a
# lot of the convenient methods (like request) come with.
#
# There's a bit of strange API surface area after we subclass - the
# connection_from_* methods are all still publicly exposed, and any caller can
# go get a connection on their own, bypassing Junction load-balancing. It may
# eventually make sense to override all of this.
#
# Because requests is built on top of urllib3 and has an insane API around
# configuring a Session, there's some extra work that gets done in our subclass
# to make it easier to support requests.

# TODO: add a junction.urllib3.request(..) fn helper, just like urllib3.request


class PoolManager(urllib3.PoolManager):
    """
    A drop-in replacement for urllib3.PoolManager that uses Junction for
    endpoint discovery and load-balancing.
    """

    def __init__(
        # urrlib3
        self,
        num_pools: int = 10,
        headers: typing.Mapping[str, str] | None = None,
        # junction
        default_routes: typing.List[junction.config.Route] | None = None,
        default_backends: typing.List[junction.config.Backend] | None = None,
        junction_client: junction.Junction | None = None,
        # kwargs
        **kwargs: typing.Any,
    ) -> None:
        if junction_client:
            self.junction = junction_client
        else:
            self.junction = junction._default_client(
                default_routes=default_routes, default_backends=default_backends
            )

        super().__init__(num_pools, headers, **kwargs)

    def urlopen(
        self,
        method: str,
        url: str,
        # NOTE: redirect is ignored
        redirect: bool = True,
        **kw: typing.Any,
    ) -> BaseHTTPResponse:
        if "headers" not in kw:
            kw["headers"] = self.headers

        # TODO: pass in defaults here - timeouts, routing rules, etc.
        endpoints = self.junction.resolve_http(
            method,
            url,
            kw["headers"],
        )

        kw["assert_same_host"] = False
        kw["redirect"] = False

        # requests has an insane API for setting TLS settings, so the junction
        # HTTP adapter has to smuggle TLS connection keys through on every
        # request instead of configuring this pool manager upfront. unsmuggle
        # them here.
        jct_tls_args = kw.pop("jct_tls_args", {})

        # TODO: actually do shadowing, etc.
        for endpoint in endpoints:
            kw2 = dict(kw)
            if endpoint.host:
                kw2["headers"]["Host"] = endpoint.host

            if endpoint.retry_policy:
                # unfortunately requests always creates an object, so testing
                # whether its a default means assuming a count of 0 is a
                # default, although we add a check on the read value being false
                # as a way to workaround when you really dont want 0 to be
                # overridden
                current = kw2.get("retries")
                is_default = not current or (current.total == 0 and not current.read)
                if is_default:
                    kw2["retries"] = urllib3.Retry(
                        total=endpoint.retry_policy.attempts - 1,
                        backoff_factor=endpoint.retry_policy.backoff,
                        status_forcelist=endpoint.retry_policy.codes,
                    )

            if (
                not kw2.get("timeout")
                and endpoint.timeout_policy
                and endpoint.timeout_policy.backend_request != 0
            ):
                kw2["timeout"] = urllib3.Timeout(
                    total=endpoint.timeout_policy.backend_request
                )
            elif (
                not kw2.get("timeout")
                and endpoint.timeout_policy
                and endpoint.timeout_policy.request != 0
            ):
                kw2["timeout"] = urllib3.Timeout(
                    ## FIXME: this is obviously not right, but urllib3 does not
                    ## give us more options. To implement properly, we would
                    ## have to implement retries ourselves.
                    total=endpoint.timeout_policy.request
                )

            conn = self.connection_from_endpoint(endpoint, **jct_tls_args)
            return conn.urlopen(
                method,
                endpoint.request_uri,
                **kw2,
            )

    def connection_from_endpoint(
        self,
        endpoint: "jct_core.Endpoint",
        **overrrides: typing.Any,
    ) -> HTTPConnectionPool:
        request_context = self._merge_pool_kwargs(overrrides)
        request_context["scheme"] = endpoint.scheme
        request_context["port"] = endpoint.addr.port

        if isinstance(endpoint.addr, endpoint.addr.SocketAddr):
            request_context["host"] = str(endpoint.addr.addr)
        elif isinstance(endpoint.addr, endpoint.addr.DnsName):
            request_context["host"] = str(endpoint.addr.name)

        if endpoint.scheme == "https":
            request_context["assert_hostname"] = endpoint.host

        return self.connection_from_context(request_context=request_context)
