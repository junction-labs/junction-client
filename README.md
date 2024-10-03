# junction-client

The Junction service discovery client libraries. These libraries enable the full
capabilities of a rich HTTP proxy, implemented via a common `junction-core` Rust
library, with wrappers on a per language and client library basis, so that very
little code needs to be changed to use its capabilities. 

At the moment Junction only supports Kubernetes services, meaning you must have
[ezbake][ezbake] installed. In the future we will add support for arbitrary DNS
services.

Proxy features supported are:
* Routing: Matching on method, path, headers, and query parameters, in line with
  the [Kubernetes Gateway API](https://gateway-api.sigs.k8s.io/).
* Timeouts
* Retries
* Splitting: weight based splitting between backend groups
* Load balancing: stateless Round Robin, and Stateful RingHash, both in line
  with the gRPC implementations.

Beyond the proxy features, a major focus of the Junction client is ensuring
first class support for configuring it in each language, particularly around
unit testing, integrating with gitops, and debugging in production.

Supported languages and client libraries are:
* [Python](#python) - the [Requests][requests] and [urllib3][urllib3] libraries

[ezbake]: https://github.com/junction-labs/ezbake
[requests]: https://pypi.org/project/requests/
[urllib3]: https://github.com/urllib3/urllib3
[gatewayapi]: https://gateway-api.sigs.k8s.io/

## Using Junction client

### Python

You must have [ezbake](https://github.com/junction-labs/ezbake) installed in
your targeted k8s cluster. 

Make sure you set up your `JUNCTION_ADS_SERVER` environment variable!

To build and install in `~/.venv`:
```bash 
cargo xtask python-build
source .venv/bin/activate
```

Then in Python, to use it to do a request:
```python
import junction.requests as requests
session = requests.Session()
resp = session.get("http://jct-http-server.default.svc.cluster.local")
resp.raise_for_status()
```

To see more on how to configure, samples are in
[junction-python/samples](./junction-python/samples/):

* [routing-and-load-balancing](./junction-python/samples/routing-and-load-balancing/README.md)
  - Traffic split and load balancing, including with dynamic configuration.
