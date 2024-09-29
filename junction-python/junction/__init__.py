import typing
from junction.junction import Junction, default_client, check_route

from . import config, requests, urllib3


##
## Purpose of this function is to convert between the non-pyo3
## route and backend structs by passing them as kwargs
##
def _get_client(
    default_routes: typing.List[config.Route] | None,
    default_backends: typing.List[config.Backend] | None,
) -> Junction:
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


__all__ = (Junction, config, urllib3, requests, check_route, default_client)
