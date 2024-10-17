import os
from typing import List, Mapping

import requests

import urllib3
from urllib3.util import Timeout
from urllib3.exceptions import HTTPError as _HTTPError
from urllib3.exceptions import InvalidHeader as _InvalidHeader
from urllib3.exceptions import ProxyError as _ProxyError
from urllib3.exceptions import SSLError as _SSLError
from urllib3.exceptions import (
    MaxRetryError,
    NewConnectionError,
    ProtocolError,
    ReadTimeoutError,
    ResponseError,
    ClosedPoolError,
    ConnectTimeoutError,
)
from .urllib3 import PoolManager
from requests.exceptions import (
    ConnectionError,
    ConnectTimeout,
    InvalidHeader,
    ProxyError,
    ReadTimeout,
    RetryError,
    SSLError,
)

import junction


class HTTPAdapter(requests.adapters.HTTPAdapter):
    """
    An HTTPAdapter subclass customized to use Junction for endpoint discovery
    and load-balancing.

    You should almost never need to use this class directly, use a Session
    instead.
    """

    def __init__(self, junction_client: junction.Junction, **kwargs):
        self.junction = junction_client
        super().__init__(**kwargs)

    def init_poolmanager(self, connections, maxsize, block=False, **pool_kwargs):
        self.poolmanager: urllib3.PoolManager = PoolManager(
            num_pools=connections,
            maxsize=maxsize,
            block=block,
            junction_client=self.junction,
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
        """Sends PreparedRequest object. Returns Response object.

        :param request: The :class:`PreparedRequest <PreparedRequest>` being sent.
        :param stream: (optional) Whether to stream the request content.
        :param timeout: (optional) How long to wait for the server to send data before giving up.
        :type timeout: float or tuple or urllib3 Timeout object
        :param verify: (optional) Either a boolean, in which case it controls whether
                we verify the server's TLS certificate, or a string, in which case it
                must be a path to a CA bundle to use
        :param cert: (optional) Any user-provided SSL certificate to be trusted.
        :param proxies: (optional) The proxies dictionary to apply to the request.
        :rtype: requests.Response
        """
        # This is overridden instead of the smaller hooks that requests provides
        # because it's where the actual load balancing takes place - for some
        # reason requests pulls connections from a PoolManager itself instead of
        # calling PoolManager.urlopen.
        #
        # The code in the original send method does:
        # - Some very basic TLS cert validation (literally just checking
        #   os.path.exists)
        # - Formats the request url so it's always relative, with a twist for
        #   proxy usage
        # - Grabs a single connection and then uses it
        # - Munges timeouts for urllib3
        # - Translates exceptions
        #
        # We're interested in fixing that middle step where the code grabs a
        # single connection and use it. Instead of doing that, grab the junction
        # PoolManager we've installed instead and delegate.

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
        elif timeout:
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

        try:
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
        except (ProtocolError, OSError) as err:
            raise ConnectionError(err, request=request)

        except MaxRetryError as e:
            if isinstance(e.reason, ConnectTimeoutError):
                # TODO: Remove this in 3.0.0: see #2811
                if not isinstance(e.reason, NewConnectionError):
                    raise ConnectTimeout(e, request=request)

            if isinstance(e.reason, ResponseError):
                raise RetryError(e, request=request)

            if isinstance(e.reason, _ProxyError):
                raise ProxyError(e, request=request)

            if isinstance(e.reason, _SSLError):
                # This branch is for urllib3 v1.22 and later.
                raise SSLError(e, request=request)

            raise ConnectionError(e, request=request)

        except ClosedPoolError as e:
            raise ConnectionError(e, request=request)

        except _ProxyError as e:
            raise ProxyError(e)

        except (_SSLError, _HTTPError) as e:
            if isinstance(e, _SSLError):
                # This branch is for urllib3 versions earlier than v1.22
                raise SSLError(e, request=request)
            elif isinstance(e, ReadTimeoutError):
                raise ReadTimeout(e, request=request)
            elif isinstance(e, _InvalidHeader):
                raise InvalidHeader(e, request=request)
            else:
                raise

        return self.build_response(request, resp)


class Session(requests.Session):
    """
    A drop-in replacement for a requests.Session that uses Junction for
    discovery and load-balancing.
    """

    def __init__(
        self,
        default_routes: List[junction.config.Route] | None = None,
        default_backends: List[junction.config.Backend] | None = None,
        junction_client: junction.Junction | None = None,
    ) -> None:
        super().__init__()

        if junction_client:
            self.junction = junction_client
        else:
            self.junction = junction._default_client(
                default_routes=default_routes, default_backends=default_backends
            )

        self.mount("https://", HTTPAdapter(junction_client=self.junction))
        self.mount("http://", HTTPAdapter(junction_client=self.junction))
