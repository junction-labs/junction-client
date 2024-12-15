import time
import typing

import urllib3
from urllib3 import BaseHTTPResponse
from urllib3.connectionpool import HTTPConnectionPool
from urllib3.exceptions import (
    TimeoutError,
    MaxRetryError,
    ResponseError,
)

import junction

from junction import Endpoint, RetryPolicy

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


def _is_redirect_err(e: MaxRetryError) -> bool:
    if not isinstance(e.reason, ResponseError):
        return False
    return "too many redirects" in e.reason.args


def _configure_retries(
    retries: urllib3.Retry | None, policy: RetryPolicy
) -> typing.Tuple[urllib3.Retry, int | bool]:
    """
    Merge Junction retries with callsite specific urilib3.Retry objects.

    Always removes redirect-based retries so that they can be handled
    directly by urllib3 instead of in the Junction wrapper.
    """
    # if nothing got specified at the callsite, use the junction policy
    #
    # unfortunately requests always creates an object, so testing whether
    # its a default means assuming a count of 0 is a default, although we
    # add a check on the read value being false as a way to workaround when
    # you really dont want 0 to be overridden
    if (not retries or not retries.total) and policy:
        retries = urllib3.Retry(
            total=policy.attempts - 1,
            backoff_factor=policy.backoff,
            status_forcelist=policy.codes,
        )
    # if there's no junction policy, use urllib3's default
    if not isinstance(retries, urllib3.Retry):
        retries = urllib3.Retry.from_int(retries)

    # strip out redirect retries now that there's a guaranteed policy
    #
    # the default of 10 was chosen extremely arbitrarily - it's the Go stdlib's
    # default and the Go authors tend to know what they're doing.
    redirect_retries = 10
    if retries.redirect is not None:
        redirect_retries = retries.redirect
        retries.redirect = None

    return retries, redirect_retries


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
        static_routes: typing.List[junction.config.Route] | None = None,
        static_backends: typing.List[junction.config.Backend] | None = None,
        junction_client: junction.Junction | None = None,
        # kwargs
        **kwargs: typing.Any,
    ) -> None:
        if junction_client:
            self.junction = junction_client
        else:
            self.junction = junction._default_client(
                static_routes=static_routes, static_backends=static_backends
            )

        super().__init__(num_pools, headers, **kwargs)

    def urlopen(
        self,
        method: str,
        url: str,
        **kwargs: typing.Any,
    ) -> BaseHTTPResponse:
        # grab defaults from self
        if "headers" not in kwargs:
            kwargs["headers"] = self.headers

        # resolve the endpoint with junction-core
        endpoint = self.junction.resolve_http(
            method,
            url,
            kwargs["headers"],
        )

        # prep for making a request by setting up kwargs once and grabbing a
        # single-ip connection pool before we do retries. retries get done
        # at this level so we can call back into junction-core without patching
        # the guts of urllib3.
        #
        # requests has an insane API for setting TLS settings, so the junction
        # HTTP adapter has to smuggle TLS connection keys through on every
        # request instead of configuring this pool manager upfront. unsmuggle
        # them here.
        jct_tls_args = kwargs.pop("jct_tls_args", {})
        kwargs["assert_same_host"] = False

        # set the host header correctly
        if endpoint.host:
            kwargs["headers"]["Host"] = endpoint.host

        # build the retry policy we'll use to make this request
        retries, redirect_retries = _configure_retries(
            kwargs.get("retries"), endpoint.retry_policy
        )

        # set a per-request timeout (don't clobber an existing one) if there's a
        # Junction timeout.
        if not kwargs.get("timeout") and endpoint.timeout_policy:
            request_timeout = min(
                endpoint.timeout_policy.backend_request, endpoint.timeout_policy.request
            )
            if request_timeout > 0:
                kwargs["timeout"] = urllib3.Timeout(total=request_timeout)

        # set up an overall deadline based on the endpoint's timeout policy
        deadline = None
        if endpoint.timeout_policy and endpoint.timeout_policy.request:
            deadline = time.time() + endpoint.timeout_policy.request

        while True:
            if deadline and time.time() > deadline:
                # TODO: should this be a Junction timeout error instead of the
                # more general urllib3 one?
                raise TimeoutError("request timeout exceeded")

            # make a copy of request kwargs and ovewrite retries to only allow
            # redirect handling.
            #
            # only setting `total=None` does weird things here
            kwargs = dict(kwargs)
            kwargs["retries"] = urllib3.Retry(
                redirect=redirect_retries,
                other=0,
                read=0,
                connect=0,
                status=0,
            )
            pool = self.connection_from_endpoint(endpoint, **jct_tls_args)

            try:
                response = pool.urlopen(method=method, url=url, **kwargs)
            except MaxRetryError as e:
                endpoint = self.junction.report_status(
                    endpoint=endpoint,
                    error=e,
                )

                if _is_redirect_err(e):
                    raise e

                # anything urllib3 thinks is worth retrying gets wrapped in
                # a MaxRetryError, even when you don't allow retries. all we
                # do here is pass that the cause of that error to our own
                # urllib3.Retry
                retries = retries.increment(
                    method, url, error=e.reason, _pool=pool, _stacktrace=e.__traceback__
                )
                retries.sleep()
                continue

            endpoint = self.junction.report_status(
                endpoint=endpoint,
                status_code=response.status,
            )
            has_retry_after = bool(response.headers.get("Retry-After"))
            if retries.is_retry(method, response.status, has_retry_after):
                try:
                    retries = retries.increment(
                        method, url, response=response, _pool=pool
                    )
                except MaxRetryError:
                    if retries.raise_on_status:
                        response.drain_conn()
                        raise
                    return response

                response.drain_conn()
                retries.sleep(response)
                continue

            return response

    def connection_from_endpoint(
        self,
        endpoint: Endpoint,
        **overrrides: typing.Any,
    ) -> HTTPConnectionPool:
        request_context = self._merge_pool_kwargs(overrrides)
        request_context["scheme"] = endpoint.scheme
        request_context["host"] = str(endpoint.addr)
        request_context["port"] = endpoint.port

        if endpoint.scheme == "https":
            request_context["assert_hostname"] = endpoint.host

        return self.connection_from_context(request_context=request_context)
