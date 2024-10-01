# junction-client

The Junction service discovery clients. The Junction clients enable the full
capabilities of a rich HTTP proxy, implemented via a common `junction-core`
library, with wrappers on a per language and library basis, so that almost no
code needs to be changed to use its capabilities. 

At the moment Junction only supports Kubernetes services meaning you must have
[ezbake][ezbake] installed. In the future we will add support for arbitrary DNS
names.

Current supported languages and their clients are:
* Python - the [Requests][requests] library

[ezbake]: https://github.com/junction-labs/ezbake
[requests]: https://pypi.org/project/requests/

## Using Junction client

You must have [ezbake][https://github.com/junction-labs/ezbake] installed in
your targeted k8s cluster.

Make sure you set up your `JUNCTION_ADS_SERVER` environment variable!

### Python

To build and install in `~/.venv`:
```bash 
cargo xtask python-build
source .venv/bin/activate
```

Then in Python, to use it to do a request:
```python
import junction.requests
session = junction.requests.Session()
resp = session.get("http://jct-http-server.default.svc.cluster.local")
resp.raise_for_status()
```

To see more on how to configure, samples are in
[junction-python/samples](./junction-python/samples/):

* [route_and_loadbalance](./junction-python/samples/route_and_loadbalance/README.md) -
  Traffic split and load balancing, including with dynamic configuration.
