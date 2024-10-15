Junction clients are part of your application and function as a cache for
dynamic configuration. All Junction clients can also be configured with static
configuration that acts as a default when a server doesn't have any dynamic
configuration for a route or a target. When you make an HTTP request, Junction
matches it against existing configuration, decides where and how to send the
request, and then hands back control to your HTTP library.

In all of our libraries, Junction clients expect to be able to talk to a dynamic
configuration server. The details of how you specify which config server to talk
to are specific to each language - we want it to feel natural.

If you don't already have a gRPC-compatible xDS server available, try installing
[ezbake](https://github.com/junction-labs/ezbake) in your local Kubernetes
cluster.

## Install `ezbake`

> **NOTE**: `ezbake` is currently not available as a pre-built container. To build
> and run it, you'll need a working Rust toolchain - we recommend installing
> [rustup](https://rustup.rs) if you haven't already.

> **NOTE**: `ezbake` doesn't do anything without a Kubernetes cluster. If you don't
> have one running, we recommend either using the cluster built into Docker Desktop or
> OrbStack, or setting up a local cluster with `k3s`.

### Building and Running in Kubernetes

First, build a container:

```bash
docker build --tag ezbake --file ./scripts/Dockerfile-develop --load .
```

By default `ezbake` requires permissions to read and watch all services in all
namespaces, but can be run in a mode where it only watches a single namespace
with the `--namespace` flag.

**NOTE**: To set up an `ezbake` with the Gateway APIs and our example policies,
you need full administrative access to your cluster. If you don't have full
access, see the advanced directions below.

The example policies in `./scripts/install-for-cluster.yml` set up a new
namespace named `junction`, a `ServiceAccount` with permissions to watch the
whole cluster, and an `ezbake` Deployment.

```bash
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml
kubectl apply -f ./scripts/install-for-cluster.yml
```

**NOTE**: These policies don't require the Gateway APIs to be defined before they're run, but
they won't actually give the ServiceAccount permission to watch or list
`HTTPRoute`s until the Gateway APIs are defined and the RBAC rules are
re-created.

To connect to your new `ezbake` with a Junction HTTP Client, use the newly
defined Kubernetes Service as your `JUNCTION_ADS_SERVER`:

```bash
export JUNCTION_ADS_SERVER="grpc://"`kubectl get svc ezbake --namespace junction -o jsonpath='{.spec.clusterIP}'`":8008"
```

To uninstall, run `kubectl delete` on the Gateway APIs and the Junction example objects:

```bash
kubectl delete -f ./scripts/install-for-cluster.yml
kubectl delete -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml
```

To install `ezbake` on a k8s cluster where you do not have full k8s administrative access, see the
advanced directions below.

## Using the client

### Python

In Python, Junction is available as a standalone client and as a drop-in replacement
for `requests.Session` or `urllib3.PoolManager`.

Using junction as a drop-in replacement for requests is as easy as:

```python
import junction.requests as requests

session = requests.Session()
resp = session.get("http://my-service.prod.svc.cluster.local")
```

To configure your client, pass `default_routes` or `default_backends` to your
`Session`. For example, to load-balance requests to an `nginx` service in the
`web` namespace based on the `x-user` header, you can specify:

```python
import junction.requests as requests

# create a new Session with a default load balancing policy for the nginx service
session = requests.Session(
    default_backends=[
        {
            "target": {"type": "service", "name": "nginx", "namespace": "web"},
            "lb": {
                "type": "RingHash",
                "hash_params": [
                    {"type": "Header", "name": "x-user"},
                ],
            },
        }
    ]
)

# make a request to the nginx service
session.get("http://nginx.web.svc.cluster.local")
```

Routes can be configured by default as well. To send all requests to `/v2/users` to a different
version of your application, you could set a default configuration like:

```python
import junction.config
import typing

import junction.requests as requests

my_service = {"type": "service", "name": "cool", "namespace": "widgets"}
my_test_service = {"type": "service", "name": "cooler", "namespace": "widgets"}

# create a new routing policy.
#
# all Junction config comes with python3 type hints, as long as you import junction.config
routes: typing.List[junction.config.Route] = [
    {
        "target": my_service,
        "rules": [
            {
                "backends": [my_test_service],
                "matches": [{"path": {"value": "/v2/users"}}],
            },
            {
                "backends": [my_service],
                "retry": retry_policy,
                "timeouts": timeouts,
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

# use the routes we just tested in our HTTP client
s = requests.Session(default_routes=routes)
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
