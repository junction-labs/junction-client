import typing
from junction.junction import Junction, default_client

from . import config, requests, urllib3


def _handle_kwargs(
    default_routes: typing.List[config.Route] | None,
    junction_client: Junction | None,
    kwargs: dict,
) -> tuple[dict, Junction]:
    if not junction_client:
        client_kwargs = {}
        if default_routes:
            client_kwargs["default_routes"] = default_routes
        junction_client = default_client(**client_kwargs)

    return kwargs, junction_client


__all__ = (Junction, config, urllib3, requests, default_client)
