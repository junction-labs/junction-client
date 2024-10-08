# routing-and-load-balancing

A test case of junction-client that works with `ezbake` and shows off routing, load balancing and
dynamic configuration capabilities.

*All paths assume you are running from the top level junction-client directory*

## Set up `ezbake` in your k8s development cluster
See https://github.com/junction-labs/ezbake/README.md

Make sure you set up your `JUNCTION_ADS_SERVER` environment variable!

## Build the junction python client
```bash
cargo xtask python-build
source .venv/bin/activate
```

## Build Server docker image and deploy it
```bash
docker build --tag jct_http_server --file junction-python/samples/routing-and-load-balancing/Dockerfile-server --load .
kubectl apply -f junction-python/samples/routing-and-load-balancing/jct_http_server.yml 
kubectl apply -f junction-python/samples/routing-and-load-balancing/jct_http_server_feature_1.yml 
```

## Run client with just client config

```bash
python junction-python/samples/routing-and-load-balancing/client.py
```

## Run client with dynamic config overriding client config

Eventually we will provide a way to export from client config to CRDs and annotations. At the moment
you have to do it by hand. To demonstrate the capability we have done so in a hard coded example,
that changes the output of one the path match sample. With this CRD in place, now 100% of traffic to
`/feature-1/index` goes to `jct-http-server-feature-1`.

```bash
kubectl apply -f junction-python/samples/routing-and-load-balancing/gateway.yml
python junction-python/samples/routing-and-load-balancing/client.py --sample path_match_sample
```

## Clean up
```bash
kubectl delete -f junction-python/samples/routing-and-load-balancing/gateway.yml
kubectl delete -f junction-python/samples/routing-and-load-balancing/jct_http_server.yml 
kubectl delete -f junction-python/samples/routing-and-load-balancing/jct_http_server_feature_1.yml 
```
