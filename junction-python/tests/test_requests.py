import typing

import pytest
import pytest_httpbin
from junction.requests import Session as JunctionSession
from requests import Response
from requests import Session as RequestsSession

SESSION_CLASSES = [RequestsSession, JunctionSession]


def _responses_equal(a: Response, b: Response):
    return (a.url == b.url) and (a.headers == b.headers) and (a.text == b.text)


def _sessions(url: str, **attrs) -> typing.List[RequestsSession]:
    set_attrs = attrs.copy()
    if url.startswith("https"):
        set_attrs["verify"] = pytest_httpbin.certs.where()

    sessions = []
    for cls in SESSION_CLASSES:
        s = cls()
        for k, v in set_attrs.items():
            setattr(s, k, v)
        sessions.append(s)

    return sessions


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
    sessions = _sessions(url)
    responses = [s.request(method, url, allow_redirects=False) for s in sessions]

    assert all(r.status_code == expected_status for r in responses)
    all(_responses_equal(responses[0], r) for r in responses)
