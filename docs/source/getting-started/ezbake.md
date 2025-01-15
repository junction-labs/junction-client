# Setting Up Control Plane - EZBake

Ezbake is a simple [xDS] control plane for
Junction, which uses the [gateway_api] to support dynamic configuration.
`ezbake` runs in a Kubernetes cluster, watches its running services, and runs
as an xDS control plane to drive the Junction client.

[ezbake]: https://github.com/junction-labs/ezbake
[gateway_api]: https://gateway-api.sigs.k8s.io/
[xDS]: https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol

## Simple Installation

The simplest installation is as follows, which first sets up the Kubernetes
Gateway API CRD, and then sets up ezbake as a 2 pod deployment in its own
namespace (junction), with permissions to monitor all services, endpoints, and
gateway API config in the cluster.

```bash
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/experimental-install.yaml
kubectl apply -f https://github.com/junction-labs/ezbake/releases/latest/download/install-for-cluster.yml
```

Now, to communicate with ezbake, all clients will need the `JUNCTION_ADS_SERVER` environment 
variable set as follows:

```bash
export JUNCTION_ADS_SERVER="grpc://ezbake.junction.svc.cluster.local:8008"
```

> [!NOTE]
>
> `ezbake` returns Pod IPs directly without any NAT, so if your cluster
> isn't configured to allow talking directly to Pod IPs from outside the cluster,
> any client you run outside the cluster **won't be able to connect to any
> backends**.  Notably, local clusters created with `k3d`, `kind`, and Docker
> Desktop behave this way.

## Uninstalling

To uninstall, run `kubectl delete` on the Gateway APIs and the objects that
`ezbake` installed:

```bash
kubectl delete -f https://github.com/junction-labs/ezbake/releases/latest/download/install-for-cluster.yml
kubectl delete -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/experimental-install.yaml
```

## More advanced installation

### Deploying to Kubernetes in a Single Namespace

On a cluster where you only have access to a single namespace, you can still run
`ezbake`. 

First you do need  your cluster admin install the Gateway APIs by [following the official instructions][official-instructions].

```bash
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/experimental-install.yaml
```

[official-instructions]: https://gateway-api.sigs.k8s.io/guides/#installing-gateway-api

Next, create a service account that has permissions to list and watch the API
server. The `ServiceAccount`, `Role` and `RoleBinding` in
`scripts/install-for-namespace-admin.yml` list all of the required privileges.
Feel free to copy that template, replace `foo` with your namespace, and apply it
to the cluster:

```bash
# run this as a cluster admin
sed 's/foo/$YOUR_NAMESPACE_HERE/' < scripts/install-for-namespace-admin.yml > ezbake-role.yml
kubectl apply -f ezbake-role.yml
```

Deploy `ezbake` as you would any other Deployment, making sure to run it as the
`ServiceAccount` created with permissions. The template in
`install-for-namespace.yml` gives an example, and can be used as a template to
get started.

```bash
sed 's/foo/$YOUR_NAMESPACE_HERE/' < scripts/install-for-namespace.yml > ezbake.yml
kubectl apply -f ezbake.yml
```
