# Installing Client - Python

The Junction client is [on PyPi](https://pypi.org/project/junction-python/). 
This means all you need to do is:

``` bash
pip install junction-python
```


## [Requests](https://pypi.org/project/requests/)

Junction is fully compatible with the Requests library, just with a different
import:

```python
import junction.requests as requests

session = requests.Session()
session.get("http://jct-simple-app.default.svc.cluster.local:8008")
```

The Junction client used by a session is also available on that session as a
field, and can be used to inspect and debug configuration.

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

The Junction client used by each PoolManager is also available as a field
and can be used to inspect and debug configuration.

```python
junction_client = http.junction
junction_client.dump_routes()
```

For more, [see the full API reference](https://docs.junctionlabs.io/api/python/stable/reference/urllib3.html).

## Direct use

Junction is generally intended to be used indirectly, though the interfaces
that that match your HTTP client. However, using the Junction client directly
can be useful to inspect and debug your configuration..

The `junction` module makes the default Junction client available for
introspection, and individual Sessions and PoolManagers make their
active clients available.

```python
import junction

client = junction.default_client()
client.dump_routes()
```

For more, [see the full API reference](https://docs.junctionlabs.io/api/python/stable/reference/junction.html#junction.Junction).

