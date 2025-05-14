import typing

from junction.junction import (
    _version,
    _build,
    Junction,
    Endpoint,
    RetryPolicy,
    default_client,
    check_route,
    dump_kube_backend,
    dump_kube_route,
    enable_tracing,
)

from . import config, requests, urllib3

__version__ = _version
__build__ = _build


def _default_client(
    static_routes: typing.Optional[typing.List[config.Route]],
    static_backends: typing.Optional[typing.List[config.Backend]],
) -> Junction:
    """
    Return a Junction client with default Routes and Backends.

    Uses the passed defaults to create a new client with default Routes
    and Backends.
    """
    client_kwargs = {}
    if static_routes:
        # This check is just in case the user does something dumb as otherwise
        # the error on the return line is pretty ambiguous
        if not isinstance(static_routes, typing.List):
            raise ValueError("static_routes must be a list of routes")
        client_kwargs["static_routes"] = static_routes
    if static_backends:
        # This check is just in case the user does something dumb as otherwise
        # the error on the return line is pretty ambiguous
        if not isinstance(static_backends, typing.List):
            raise ValueError("static_backends must be a list of backends")
        client_kwargs["static_backends"] = static_backends
    return default_client(**client_kwargs)


__all__ = (
    Junction,
    Endpoint,
    RetryPolicy,
    config,
    urllib3,
    requests,
    check_route,
    default_client,
    dump_kube_backend,
    dump_kube_route,
    enable_tracing,
)
