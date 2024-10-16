import typing
from junction.junction import (
    Junction,
    default_client,
    check_route,
    dump_kube_backend,
    dump_kube_route,
)

from . import config, requests, urllib3


def _default_client(
    default_routes: typing.List[config.Route] | None,
    default_backends: typing.List[config.Backend] | None,
) -> Junction:
    """
    Return a Junction client with default Routes and Backends.

    Uses the passed defaults to create a new client with default Routes
    and Backends.
    """
    client_kwargs = {}
    if default_routes:
        # This check is just in case the user does something dumb as otherwise
        # the error on the return line is pretty ambiguous
        if not isinstance(default_routes, typing.List):
            raise ValueError("default_routes must be a list of routes")
        client_kwargs["default_routes"] = default_routes
    if default_backends:
        # This check is just in case the user does something dumb as otherwise
        # the error on the return line is pretty ambiguous
        if not isinstance(default_backends, typing.List):
            raise ValueError("default_backends must be a list of backends")
        client_kwargs["default_backends"] = default_backends
    return default_client(**client_kwargs)


__all__ = (
    Junction,
    config,
    urllib3,
    requests,
    check_route,
    default_client,
    dump_kube_backend,
    dump_kube_route,
)
