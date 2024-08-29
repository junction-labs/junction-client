import os
from typing import Mapping

import requests
import urllib3
from urllib3.util import Timeout

import junction

from .urllib3 import PoolManager


class HTTPAdapter(requests.adapters.HTTPAdapter):
    """
    An HTTPAdapater subclass customized to use Junction for endpoint discovery
    and load-balancing.

    You almost never need to use this class directly, use a Session instead.
    """

    def __init__(self, **kwargs):
        self._jct_kwargs = kwargs.pop("jct_kwargs")
        super().__init__(**kwargs)

    def init_poolmanager(self, connections, maxsize, block=False, **pool_kwargs):
        pool_kwargs.update(self._jct_kwargs)

        self.poolmanager: urllib3.PoolManager = PoolManager(
            num_pools=connections,
            maxsize=maxsize,
            block=block,
            **pool_kwargs,
        )

    def send(
        self,
        request: requests.PreparedRequest,
        stream: bool = False,
        timeout: None | float | tuple[float, float] | tuple[float, None] = None,
        verify: bool | str = True,
        cert: None | bytes | str | tuple[bytes | str, bytes | str] = None,
        proxies: Mapping[str, str] | None = None,
    ) -> requests.Response:
        # This is overridden instead of the smaller hooks that requests provides because
        # it's where the actual load balancing takes place - for some reason requests
        # pulls connections from a PoolManager itself instead of calling PoolManager.urlopen.
        #
        # The code in the original send method does:
        # - Some very basic TLS cert validation (literally just checking os.path.exists)
        # - Formats the request url so it's always relative, with a twist for proxy usage
        # - Grabs a single connection and then uses it
        # - Munges timeouts for urllib3
        # - Translates exceptions
        #
        # We're interested in fixing that middle step where the code grabs a single connection
        # and use it. Instead of doing that, grab the junction PoolManager we've installed
        # instead and delegate.

        self.add_headers(
            request,
            stream=stream,
            timeout=timeout,
            verify=verify,
            cert=cert,
            proxies=proxies,
        )
        chunked = not (request.body is None or "Content-Length" in request.headers)

        # duplicate requests' timeout handling. it's gotta eventually be a
        # urllib3.util.Timeout so make it one.
        if isinstance(timeout, tuple):
            try:
                connect, read = timeout
                timeout = Timeout(connect=connect, read=read)
            except ValueError:
                raise ValueError(
                    f"Invalid timeout {timeout}. Pass a (connect, read) timeout to "
                    f"configure both timeouts individually or a single float to set "
                    f"both timeouts to the same value"
                ) from None
        elif isinstance(timeout, Timeout):
            pass
        else:
            timeout = Timeout(connect=timeout, read=timeout)

        # in requests.HTTPAdapter, TLS settings get configured on individual
        # connections fetched from the pool. since there's no connection
        # fetching going on here, make sure to pass tls args on every request.
        #
        # for now this involves smuggling it through urlopen's kwargs, but there
        # may be a way to do this by resetting the existing poolmanager whenever
        # session.verify is set with some cursed # setattr nonsense. requests
        # itself does some tls context building, so sneak this through for now
        # and we can figure this out later.
        tls_args = {
            "cert_reqs": "CERT_REQUIRED",
        }
        if verify is False:
            tls_args["cert_reqs"] = "CERT_NONE"
        elif isinstance(verify, str):
            if os.path.isdir(verify):
                tls_args["ca_cert_dir"] = verify
            else:
                tls_args["ca_certs"] = verify

        # TODO: catch and translate exceptions back into requests exceptions
        resp = self.poolmanager.urlopen(
            method=request.method,
            url=request.url,
            redirect=False,
            body=request.body,
            headers=request.headers,
            assert_same_host=False,
            preload_content=False,
            decode_content=False,
            retries=self.max_retries,
            timeout=timeout,
            chunked=chunked,
            jct_tls_args=tls_args,
        )

        return self.build_response(request, resp)


class Session(requests.Session):
    """
    A drop-in replacement for a requests.Session that uses Junction for
    discovery and load-balancing.
    """

    def __init__(self, **kwargs) -> None:
        super().__init__()

        jct_kwargs = {}
        for key in junction._KWARG_NAMES:
            if value := kwargs.pop(key, None):
                jct_kwargs[key] = value

        self.mount("https://", HTTPAdapter(jct_kwargs=jct_kwargs))
        self.mount("http://", HTTPAdapter(jct_kwargs=jct_kwargs))
