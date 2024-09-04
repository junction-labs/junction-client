import typing

import pytest
import pytest_httpbin
from urllib3 import BaseHTTPResponse
from urllib3 import PoolManager as Urllib3PoolManager

from junction.urllib3 import PoolManager as JunctionPoolManger

POOL_CLASSES = [Urllib3PoolManager, JunctionPoolManger]


def _responses_equal(a: BaseHTTPResponse, b: BaseHTTPResponse):
    return (a.url == b.url) and (a.headers == b.headers) and (a.data == b.data)


def _pool_managers(url: str, **kwargs) -> typing.List[Urllib3PoolManager]:
    pool_kwargs = kwargs.copy()
    if url.startswith("https"):
        pool_kwargs["ca_certs"] = pytest_httpbin.certs.where()

    return [
        pool_cls(ca_certs=pytest_httpbin.certs.where()) for pool_cls in POOL_CLASSES
    ]


@pytest.mark.skip(reason="figure out test ADS server")
@pytest.mark.parametrize(
    "method,path,expected_status",
    [
        ("GET", "/json", 200),
        ("GET", "/status/204", 204),
        ("GET", "/status/301", 301),
        ("GET", "/status/404", 404),
        ("DELETE", "/status/204", 204),
    ],
)
def test_no_request_body(httpbin_both, method, path, expected_status):
    url = httpbin_both.url + path
    pool_managers = _pool_managers(url)
    responses = [p.urlopen(method, url, redirect=False) for p in pool_managers]

    assert all(r.status == expected_status for r in responses)
    all(_responses_equal(responses[0], r) for r in responses)


@pytest.mark.skip(reason="figure out test ADS server")
def test_post_json(httpbin_both):
    url = httpbin_both.url + "/post"
    pool_managers = _pool_managers(url)

    for p in pool_managers:
        resp = p.request("POST", url, json={"drink": "coffee"})
        resp_body = resp.json()

        assert resp_body["json"] == {"drink": "coffee"}
