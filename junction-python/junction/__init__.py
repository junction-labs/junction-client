from junction.junction import Junction, default_client

from . import requests, urllib3

_KWARG_NAMES = [
    "default_routes",
]


def _handle_kwargs(kwargs: dict) -> tuple[dict, Junction]:
    jct_kwargs = {}
    for key in _KWARG_NAMES:
        if value := kwargs.pop(key, None):
            jct_kwargs[key] = value

    jct_client = kwargs.pop("junction_client", None)
    if not jct_client:
        jct_client = default_client(**jct_kwargs)

    return kwargs, jct_client


__all__ = (Junction, urllib3, requests, default_client)
