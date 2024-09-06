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
# the connection_from_* methods to pick out a connection for the endpoints
# we'd like to send data to. urlopen is a nice place to do that, because it means
# a lot of the convenient methods (like request) come with.
#
# There's a bit of strange API surface area after we subclass - the connection_from_*
# methods are all still publicly exposed, and any caller can go get a connection
# on their own, bypassing Junction load-balancing. It may eventually make sense
# to override all of this.
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
        self,
        num_pools: int = 10,
        headers: typing.Mapping[str, str] | None = None,
        **kwargs: typing.Any,
    ) -> None:
        connection_pool_kw, client = junction._handle_kwargs(kwargs)
        self.junction = client

        super().__init__(num_pools, headers, **connection_pool_kw)

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
        endpoints = self.junction.resolve_endpoints(
            method,
            url,
            kw["headers"],
        )

        kw["assert_same_host"] = False
        kw["redirect"] = False

        # requests has an insane API for setting TLS settings, so the junction
        # HTTP adapter has to smuggle TLS connection keys through on every request
        # instead of configuring this pool manager upfront. unsmuggle them here.
        jct_tls_args = kw.pop("jct_tls_args", {})

        # TODO: add timeout fields to endpoints
        # TODO: should scheme coome from the endpoint?
        # TODO: actually do shadowing, etc.
        for endpoint in endpoints:
            if endpoint.host:
                kw["headers"]["Host"] = endpoint.host

            conn = self.connection_from_endpoint(endpoint, **jct_tls_args)
            return conn.urlopen(
                method,
                endpoint.request_uri,
                **kw,
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
