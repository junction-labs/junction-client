Junction is a control-plane driven client library for service routing, load-balancing, and resilience.

## About

Junction is a library that allows you to dynamically configure application level
HTTP routing, load balancing, and resilience by writing a few lines of code.
Junction is like having a rich HTTP proxy embedded in your client, with deep
integrations into the libraries you already use.

Junction can be statically configured, with plain old software, or dynamically
configured with any gRPC-xDS compatible configuration server. Check out [ezbake]
for a simple server that lets you save Junction config using the
[Kubernetes Gateway API][gateway_api]

[ezbake]: https://github.com/junction-labs/ezbake
[gateway_api]: https://gateway-api.sigs.k8s.io/

### Project status

Junction is alpha software, developed in the open. We're
still iterating rapidly on core code and APIs. At this stage you should expect
occasional breaking changes as the library evolves. At the moment, Junction only
supports talking to Kubernetes services, with support for routing to any
existing DNS name coming very soon.

### Features

Junction allows you to statically or dynamically configure:

* Routing traffic to backends based on the HTTP method, path, headers, or query parameters.
* Timeouts
* Retries
* Weighted traffic splitting
* Load balancing

Junction differs from existing libraries and proxies is that we aim to do all of
this in your native programming language, with low overhead and deep integration
into your existing HTTP client. This approach means that Junction can provide
dynamic config, but still provide first class support for things like unit testing
and debugging, putting developers back in control of their applications.

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
