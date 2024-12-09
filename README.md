# junction-client

An embeddable, dynamically configurable HTTP library.

## About

Junction is a library that allows you to dynamically configure application level
HTTP routing, load balancing, and resilience by writing a few lines of
configuration and dynamically pushing it to your client. Imagine having all of
the features of a rich HTTP proxy that's as easy to work with as the HTTP
library you're already using.

Junction can be statically configured, with plain old software, or dynamically
configured with any gRPC-xDS compatible configuration server. Check out [ezbake]
for a simple server that lets you save Junction config using the [Kubernetes
Gateway API][gateway_api]

[ezbake]: https://github.com/junction-labs/ezbake
[gateway_api]: https://gateway-api.sigs.k8s.io/

### Project status

Junction is alpha software, developed in the open. We're still iterating rapidly
on core code and APIs. At this stage you should expect occasional breaking
changes as the library evolves. At the moment, Junction only supports talking to
Kubernetes services, with support for routing to any existing DNS name coming
very soon.

### Features

Junction allows you to statically or dynamically configure:

* Routing traffic to backends based on the HTTP method, path, headers, or query parameters.
* Timeouts
* Retries
* Weighted traffic splitting
* Load balancing

Junction differs from existing libraries and proxies in that we aim to do all of
this in your native programming language, with low overhead and deep integration
into your existing HTTP client. This approach means that Junction can provide
dynamic config, but still provide first class support for things like unit
testing and debugging, putting developers back in control of their applications.

### Roadmap

We're starting small and focused with Junction. Our first goal is getting
configuration right. Our next goal is allowing you to push configuration that
you've developed and tested to a centralized config server.

### Supported Languages and HTTP Libraries

| Language | Core Library      | Supported HTTP Libraries |
|----------|-------------------|--------------------------|
| Rust     | `junction-core`   |  None                    |
| Python   | `junction`        | [requests] and [urllib3] |

[requests]: https://pypi.org/project/requests/
[urllib3]: https://github.com/urllib3/urllib3

## Using a Junction client

Junction clients are part of your application and function as a cache for
dynamic configuration. All Junction clients can also be configured with static
configuration that acts as a default when a server doesn't have any dynamic
configuration for a route or a target. When you make an HTTP request, Junction
matches it against existing configuration, decides where and how to send the
request, and then hands back control to your HTTP library.

In all of our libraries, Junction clients expect to be able to talk to a dynamic
configuration server. The details of how you specify which config server to talk
to are specific to each language - we want it to feel natural. For now, the
easiest control plane to use is
[ezbake](https://github.com/junction-labs/ezbake), our sample self-hosted
control plane.

### Python

#### Making Requests

In Python, Junction is available as a standalone client and as a drop-in replacement
for `requests.Session` or `urllib3.PoolManager`.

Using junction as a drop-in replacement for requests is as easy as:

```python
import junction.requests as requests

session = requests.Session()
resp = session.get("http://my-service.prod.svc.cluster.local")
```

Just by creating a client and making requests with a Junction session, you're using
dynamic discovery and load-balancing.

#### Creating Config

Setting up Junction Routes and Backends is almost as easy as using the client.
For example, if you wanted to shard a memcached cluster by your internal user-id,
you'd declare it like this:

```python
memcached = {
    "id": {"type": "kube", "name": "nginx", "namespace": "web"},
    "lb": {
        "type": "RingHash",
        "hash_params": [
            {"type": "Header", "name": "x-user"},
        ],
    },
}
```

Junction includes typing information for both Routes and Backends. Because Junction
fully runs in your client, you can unit test your Routes as you create them with no
network connection required.

```python
import junction.config
import typing

my_service = {"type": "kube", "name": "cool", "namespace": "widgets"}
my_test_service = {"type": "kube", "name": "cooler", "namespace": "widgets"}

# create a retry policy that can be re-used for
retry_policy: junction.config.RouteRetry = {
    "attempts": 3,
    "backoff": 0.5,
    "codes": [500, 503],
}

# create a new routing policy.
#
# all Junction config comes with python3 typing info
routes: typing.List[junction.config.Route] = [
    {
        "id": "my-route",
        "hostnames": ["cool.widgets.svc.cluster.local"],
        "rules": [
            {
                "backends": [my_test_service],
                "retry": retry_policy,
                "matches": [{"path": {"value": "/v2/users"}}],
            },
            {
                "backends": [my_service],
                "retry": retry_policy,
            },
        ],
    },
]


# assert that requests with no path go to the cool service like normal
(_, _, matched_backend) = junction.check_route(
    routes, "GET", "http://cool.widgets.svc.cluster.local", {}
)
assert matched_backend["name"] == "cool"

# assert that requests to /v2/users go to an even cooler service
(_, _, matched_backend) = junction.check_route(
    routes, "GET", "http://cool.widgets.svc.cluster.local/v2/users", {}
)
assert matched_backend["name"] == "cooler"
```

For a more complete example configuration see the [sample] project. It's a
runnable example containing routing tables, load balancing, retry policies, and
more.

[sample]: https://github.com/junction-labs/junction-client/tree/main/junction-python/samples/routing-and-load-balancing

### NodeJS

NodeJS support is coming soon! Please reach out to
[info@junctionlabs.io](mailto:info@junctionlabs.io) if you run NodeJS services
and are interested in being an early access design partner.

### Rust

The core of Junction is written in Rust and is available in the
[`junction-core`](https://github.com/junction-labs/junction-client/tree/main/crates/junction-core)
crate. At the moment, we don't have an integration with an HTTP library
available, but you can use the core client to dynamically fetch config and
resolve addresses.

See the `examples` directory for [an
example](https://github.com/junction-labs/junction-client/blob/main/crates/junction-core/examples/get-endpoints.rs)
of how to use junction to resolve an address.
