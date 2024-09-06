import os
import typing

import pytest
from urllib3 import BaseHTTPResponse
from urllib3 import PoolManager as Urllib3PoolManager

from junction.urllib3 import PoolManager as JunctionPoolManger

POOL_CLASSES = [Urllib3PoolManager, JunctionPoolManger]


def _responses_equal(a: BaseHTTPResponse, b: BaseHTTPResponse):
    return (a.url == b.url) and (a.headers == b.headers) and (a.data == b.data)


def _pool_managers(url: str, **kwargs) -> typing.List[Urllib3PoolManager]:
    return [pool_cls() for pool_cls in POOL_CLASSES]


@pytest.mark.skipif(
    "JUNCTION_ADS_SERVER" not in os.environ,
    reason="missing ADS server address",
)
def test_no_request_body():
    url = "http://nginx.default.svc.cluster.local"
    pool_managers = _pool_managers(url)
    responses = [p.urlopen("GET", url, redirect=False) for p in pool_managers]

    assert all(r.status == 200 for r in responses)
    all(_responses_equal(responses[0], r) for r in responses)
