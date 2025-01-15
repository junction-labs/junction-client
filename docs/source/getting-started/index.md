# Overview

There are 3 steps to getting Junction running.

## Step 1 - Set up a control plane

Today the only xDS server the junction-client regression tests against is
[ezbake]. Ezbake is a simple [xDS] control plane for
Junction, which uses the [gateway_api] to support dynamic configuration.
`ezbake` runs in a Kubernetes cluster, watches its running services, and runs
as an xDS control plane to drive the Junction client.

[ezbake]: https://github.com/junction-labs/ezbake
[gateway_api]: https://gateway-api.sigs.k8s.io/
[xDS]: https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol

* [Set up ezbake](ezbake.md)

## Step 2 - Install the Junction client

Here you just need to read the guide for the languages you are using.

* [Node.js](node.md)
* [Python](python.md)
* [Rust](rust.md)

## Step 3 - Configure your client behavior

Finally you must configure Junction to shape your clients behavior.

* [Configuring Junction](configuring-junction.md)
