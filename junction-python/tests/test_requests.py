import typing

from requests import Response
from requests import Session as RequestsSession

from junction.requests import Session as JunctionSession

SESSION_CLASSES = [RequestsSession, JunctionSession]


def _responses_equal(a: Response, b: Response):
    return (a.url == b.url) and (a.headers == b.headers) and (a.text == b.text)


def _sessions(url: str, **attrs) -> typing.List[RequestsSession]:
    return [cls() for cls in SESSION_CLASSES]
