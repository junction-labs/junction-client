import typing
from junction.junction import Junction, default_client

from . import config, requests, urllib3


def _handle_kwargs(
    default_routes: typing.List[config.Route] | None,
    default_backends: typing.List[config.Backend] | None,
    junction_client: Junction | None,
    kwargs: dict,
) -> tuple[dict, Junction]:
    if not junction_client:
        client_kwargs = {}
        if default_routes:
            client_kwargs["default_routes"] = default_routes
        if default_backends:
            client_kwargs["default_backends"] = default_backends
        junction_client = default_client(**client_kwargs)
    return kwargs, junction_client


__all__ = (Junction, config, urllib3, requests, default_client)
