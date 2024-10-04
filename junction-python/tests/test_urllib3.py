import typing

from urllib3 import BaseHTTPResponse
from urllib3 import PoolManager as Urllib3PoolManager

from junction.urllib3 import PoolManager as JunctionPoolManger

POOL_CLASSES = [Urllib3PoolManager, JunctionPoolManger]


def _responses_equal(a: BaseHTTPResponse, b: BaseHTTPResponse):
    return (a.url == b.url) and (a.headers == b.headers) and (a.data == b.data)


def _pool_managers(url: str, **kwargs) -> typing.List[Urllib3PoolManager]:
    return [pool_cls() for pool_cls in POOL_CLASSES]
