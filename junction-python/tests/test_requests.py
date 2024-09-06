import os
import typing

import pytest
from requests import Response
from requests import Session as RequestsSession

from junction.requests import Session as JunctionSession

SESSION_CLASSES = [RequestsSession, JunctionSession]


def _responses_equal(a: Response, b: Response):
    return (a.url == b.url) and (a.headers == b.headers) and (a.text == b.text)


def _sessions(url: str, **attrs) -> typing.List[RequestsSession]:
    return [cls() for cls in SESSION_CLASSES]


@pytest.mark.skipif(
    "JUNCTION_ADS_SERVER" not in os.environ,
    reason="missing ADS server address",
)
def test_no_request_body():
    url = "http://nginx.default.svc.cluster.local"
    sessions = _sessions(url)
    responses = [s.request("GET", url, allow_redirects=False) for s in sessions]

    assert all(r.status_code == 200 for r in responses)
    all(_responses_equal(responses[0], r) for r in responses)
