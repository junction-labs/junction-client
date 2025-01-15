# Installing Client - Python

The Junction client is [on PyPi](https://pypi.org/project/junction-python/). 
This means all you need to do is:

``` bash
pip install junction-python
```

## Direct use

To use the junction client for configuring/debugging, you can then invoke it
via:

```python
import junction
junction_client = junction.default_client()
junction_client.dump_routes()
```

For more, [see the full API reference](https://docs.junctionlabs.io/api/python/stable/reference/junction.html#junction.Junction).

However, Junction is generally intended to be used indirectly, though interfaces
that that match existing HTTP clients. These are covered in the following
sections.

## [Requests](https://pypi.org/project/requests/)

Junction is fully compatible with the Requests library, just with a different
import:

```python
import junction.requests as requests

session = requests.Session()
session.get("http://jct-simple-app.default.svc.cluster.local:8008")
```

We do also monkey patch in a way to get to the client used by a session:

```python
junction_client = session.junction
junction_client.dump_routes()
```

For more, [see the full API reference](https://docs.junctionlabs.io/api/python/stable/reference/requests.html).

## [Urllib3](https://github.com/urllib3/urllib3)

Junction is fully compatible with the Urllib3 library, just with a different
import:

```python
from junction.urllib3 import PoolManager
http = PoolManager()
http.urlopen("GET", "http://jct-simple-app.default.svc.cluster.local:8008")
```

We do also monkey patch in a way to get to the client used by a session:

```python
junction_client = http.junction
junction_client.dump_routes()
```

For more, [see the full API reference](https://docs.junctionlabs.io/api/python/stable/reference/urllib3.html).
