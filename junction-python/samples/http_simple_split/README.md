# http_simple_split

A test case of junction-client that works with `ezbake` and traffic-director.

*All paths assume you are running from the top level junction-client directory*

## Using junction-client with `ezbake`

### Set up `ezbake` and the junction python client
To build the junction python client and set up a venv for it:
```bash
cargo xtask python-build
source .venv/bin/activate
```

For ezbake see https://github.com/junction-labs/ezbake/README.md

Once setup, you then need to set up the environment variable pointing to it:
```bash
export JUNCTION_ADS_SERVER="grpc://"`kubectl get svc ezbake --namespace junction -o jsonpath='{.spec.clusterIP}'`":8008"
```

### Build Server docker image
```bash
docker build --tag jct_http_server --file junction-python/samples/http_simple_split/Dockerfile --load junction-python/samples/http_simple_split/
```

### Deploy Servers
```bash
  kubectl apply -f junction-python/samples/http_simple_split/k8s_jct_http_server.yml 
  kubectl apply -f junction-python/samples/http_simple_split/k8s_jct_http_server_feature_1.yml 
```

### Run client using client config

```bash
  python junction-python/samples/http_simple_split/jct_http_client.py
```

### Run client using dynamic config

For dynamic config you first need to set up the gateway API CRDs
as per the expake config. You can then install the dynamic config.

```bash
  kubectl apply -f junction-python/samples/http_simple_split/k8s_gateway.yml 
```

Then to run the client:

```bash
  python junction-python/samples/http_simple_split/jct_http_client.py --session no-client-config
```

### Clean up
```bash
  kubectl delete -f junction-python/samples/http_simple_split/k8s_jct_http_server.yml 
  kubectl delete -f junction-python/samples/http_simple_split/k8s_jct_http_server_feature_1.yml 
```
