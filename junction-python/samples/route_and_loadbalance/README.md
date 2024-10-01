# route_and_loadbalance

A test case of junction-client that works with `ezbake` and show off routing,
load balancing and dynamic configuration capabilities.

*All paths assume you are running from the top level junction-client directory*

## Using junction-client with `ezbake`

### Set up `ezbake` in your k8s development cluster
See https://github.com/junction-labs/ezbake/README.md

Make sure you set up your `JUNCTION_ADS_SERVER` environment variable!

### Build the junction python client
```bash
cargo xtask python-build
source .venv/bin/activate
```

### Build Server docker image and deploy it
```bash
docker build --tag jct_http_server --file junction-python/samples/route_and_loadbalance/Dockerfile --load junction-python/samples/route_and_loadbalance/
kubectl apply -f junction-python/samples/route_and_loadbalance/k8s_jct_http_server.yml 
kubectl apply -f junction-python/samples/route_and_loadbalance/k8s_jct_http_server_feature_1.yml 
```

### Run client with just client config

```bash
python junction-python/samples/route_and_loadbalance/jct_http_client.py
```

You should see something like:
```bash
***** Responses for Path: '/index', Headers: '{}' *****
  version:jct-http-server, server_id:1: 34
  version:jct-http-server, server_id:48: 33
  version:jct-http-server, server_id:85: 33
***** Responses for Path: '/index', Headers: '{'USER': 'inowland'}' *****
  version:jct-http-server, server_id:1: 33
  version:jct-http-server, server_id:48: 33
  version:jct-http-server, server_id:85: 34
***** Responses for Path: '/feature-1/index', Headers: '{}' *****
  version:jct-http-server, server_id:1: 15
  version:jct-http-server, server_id:48: 15
  version:jct-http-server, server_id:85: 15
  version:jct-http-server-feature-1, server_id:17: 15
  version:jct-http-server-feature-1, server_id:39: 26
  version:jct-http-server-feature-1, server_id:60: 14
***** Responses for Path: '/feature-1/index', Headers: '{'USER': 'inowland'}' *****
  version:jct-http-server, server_id:1: 17
  version:jct-http-server, server_id:48: 17
  version:jct-http-server, server_id:85: 17
  version:jct-http-server-feature-1, server_id:39: 49
```

Explaining this:
* `/index` does not get split and goes 100% to `jct-http-server`
* `/feature-1/index` gets 50/50 split between `jct-http-server` and
  `jct-http-server-feature-1`
* `jct-http-server-feature-1` also has consistent hashing if the `"USER"` header
  is set, so all of its traffic goes to a single server

### Run client with dynamic config overriding client config

Eventually we will provide a way to export from client config to CRDs and
annotations. At the moment you hace to do it by hand. To demonstrate the
capability we have done so in a hard coded config:

```bash
kubectl apply -f junction-python/samples/route_and_loadbalance/k8s_gateway.yml
python junction-python/samples/route_and_loadbalance/jct_http_client.py
```

You should now see something like 
```bash
***** Responses for Path: '/index', Headers: '{}' *****
  version:jct-http-server, server_id:1: 34
  version:jct-http-server, server_id:48: 33
  version:jct-http-server, server_id:85: 33
***** Responses for Path: '/index', Headers: '{'USER': 'inowland'}' *****
  version:jct-http-server, server_id:1: 33
  version:jct-http-server, server_id:48: 33
  version:jct-http-server, server_id:85: 34
***** Responses for Path: '/feature-1/index', Headers: '{}' *****
  version:jct-http-server-feature-1, server_id:17: 29
  version:jct-http-server-feature-1, server_id:39: 34
  version:jct-http-server-feature-1, server_id:60: 37
***** Responses for Path: '/feature-1/index', Headers: '{'USER': 'inowland'}' *****
  version:jct-http-server-feature-1, server_id:39: 100
```

Now, 100% of traffic to `/feature-1/index` goes to `jct-http-server-feature-1`.

### Clean up
```bash
kubectl delete -f junction-python/samples/route_and_loadbalance/k8s_gateway.yml
kubectl delete -f junction-python/samples/route_and_loadbalance/k8s_jct_http_server.yml 
kubectl delete -f junction-python/samples/route_and_loadbalance/k8s_jct_http_server_feature_1.yml 
```
