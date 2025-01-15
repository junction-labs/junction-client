# Configuring Junction

Junction enables testing to validate behavior with the same configuration that
eventually gets used dynamically. In this last part of the getting started
guide, we walk through how the same configuration can be used in three different
ways.

* Static configuration for unit testing
* Static configuration with dynamic IP's for Integration Testing
* Dynamic configuration with ezbake

## Defining our config

To start, we need to define a configuration. Here we have a route on the
hostname `jct-simple-app.default.svc.cluster.local`, which sends all requests
matching the path `/v2/user` to a test service called `jct-simple-app-v2`,
putting in place a retry policy as well:

```python
import junction
import junction.config
import typing

my_service = {"type": "kube", "name": "jct-simple-app", "namespace": "default"}
my_test_service = {"type": "kube", "name": "jct-simple-app-v2", "namespace": "default"}

retry_policy: junction.config.RouteRetry = {
    "attempts": 3,
    "backoff": 0.5,
    "codes": [500, 503],
}

routes: typing.List[junction.config.Route] = [
    {
        "id": "jct-simple-app-routes",
        "hostnames": ["jct-simple-app.default.svc.cluster.local"],
        "rules": [
            {
                "backends": [{**my_test_service, "port": 8008}],
                "retry": retry_policy,
                "matches": [{"path": {"value": "/v2/users"}}],
            },
            {
                "backends": [{**my_service, "port": 8008}],
                "retry": retry_policy,
            },
        ],
    },
]
```

## Static configuration for unit testing

The first step in testing a configuration is unit tests. Junction provides the
check_route method, which lets you test how a specific request will get
processed by it's rule:

```python
# assert that requests with no path go to the cool service like normal
(_, _, matched_backend) = junction.check_route(
    routes, "GET", "http://jct-simple-app.default.svc.cluster.local", {}
)
assert matched_backend["name"] == "jct-simple-app"

# assert that requests to /v2/users go to the cool-test service
(_, _, matched_backend) = junction.check_route(
    routes, "GET", "http://jct-simple-app.default.svc.cluster.local/v2/users", {}
)
assert matched_backend["name"] == "jct-simple-app-v2"
```

## Static configuration for pre-deployment testing

Before we roll out the configuration dynamically, we probably want to see it
work in a real HTTP client. To allow this mode, all clients can be configured
with static routes and backends, and use those rather than what comes back from
the control plane.

Then:
```python
import junction.requests as requests

session = junction.requests.Session(
    static_routes=routes
)
r1 = session.get("http://jct-simple-app.default.svc.cluster.local")
r2 = session.get("http://jct-simple-app.default.svc.cluster.local/v2/users")
# both go to expected service
print(r1.text)
print(r2.text)
```

## Dynamic configuration with ezbake

The final step is deploying the configuration to the control plane feeding all
clients in the cluster. Junction provides integrations with EZBake to make this
simple. EZBake uses the Gateway API [HTTPRoute] to specify routes, and so
junction provides a method that allows configuration to be dumped to a file,
that can either be directly executed with `kubectl apply -f`, or put in whatever
GitOps mechanism you use to roll out configuration. 

[HTTPRoute]: https://gateway-api.sigs.k8s.io/api-types/httproute/

```python
for route in routes:
    print("---")
    print(junction.dump_kube_route(route=route, namespace="default"))
```
