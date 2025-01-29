# Overview 

An xDS dynamically-configurable API load-balancer library.

## What is it?

Junction is a library that allows you to dynamically configure application 
level HTTP routing, load balancing, and resilience by writing a few lines of
configuration and dynamically pushing it to your client. Imagine all of the
features of a rich HTTP proxy that's as easy to work with as the HTTP library
you're already using. 

Junction does that by pulling endpoints and configuration from an [xDS] 
control plane, as follows:

[xDS]: https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol


```
┌─────────────────────┐                 
│    Client Service   │    
├──────────┬──────────┤    ┌───────────┐
│ Existing │ Junction ◄────┤    xDS    │
│   HTTP   │  Client  │    │  Control  │
│ Library  │ Library  │    │   Plane   │
└────┬─┬───┴──────────┘    └─────▲─────┘
     │ │                         │      
     │ └──────────┐              │      
┌────▼────┐  ┌────▼────┐   ┌─────┴─────┐
│  Your   │  │  Your   │   │  K8s API  │
│ Service │  │ Service │   │  Server   │
└─────────┘  └─────────┘   └───────────┘
```

Junction is developed by [Junction Labs](https://www.junctionlabs.io/).

## Features

Today, Junction allows you to dynamically configure:

- Routing traffic based on HTTP method, path, headers, or query parameters
- Timeouts
- Retries
- Weighted traffic splitting
- Load balancing (Ring-Hash or WRR)

On our roadmap are features like:

- multi-cluster federation
- zone-based load balancing
- rate limiting
- subsetting
- circuit breaking

## Supported xDS Control Planes

Today the only xDS server the junction-client regression tests against is
[ezbake], developed by Junction Labs. [ezbake] is a simple xDS control plane for
Junction, which uses the [gateway_api] to support dynamic configuration.
`ezbake` runs in a Kubernetes cluster, watches its running services, and creates
the xds configuration to drive the Junction Client.

[ezbake]: https://github.com/junction-labs/ezbake
[gateway_api]: https://gateway-api.sigs.k8s.io/

## Supported languages and HTTP Libraries

| Language    | Integrated HTTP Libraries |
|-------------|---------------------------|
| [Rust]      | None                      |
| [Python]    | [requests], [urllib3]     |
| [Node.js]   | [fetch()]                 |

[Rust]: https://docs.junctionlabs.io/getting-started/rust.md
[Python]: https://docs.junctionlabs.io/getting-started/python.md
[Node.js]: https://docs.junctionlabs.io/getting-started/node.md
[requests]: https://pypi.org/project/requests/
[urllib3]: https://github.com/urllib3/urllib3
[fetch()]: https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API/Using_Fetch

## Getting started

[See here](https://docs.junctionlabs.io/getting-started/)

## Project status

Junction is alpha software, developed in the open. We're still iterating rapidly
on our client facing API, and our integration into xDS. At this stage you should 
expect occasional breaking changes as the library evolves. 

## License

The Junction client is [Apache 2.0 licensed](https://github.com/junction-labs/junction-client/blob/main/LICENSE).

## Contact Us

[info@junctionlabs.io](mailto:info@junctionlabs.io)
