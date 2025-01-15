# smoke-test

A test case of junction-client that works with `ezbake` and shows off routing, load balancing and
dynamic configuration capabilities.

*All paths assume you are running from the top level junction-client directory*

## Build the junction python client and Set up the environment
```bash
cargo xtask python-build
source .venv/bin/activate
```

## Set up `ezbake` 
```bash
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/experimental-install.yaml
kubectl apply -f https://github.com/junction-labs/ezbake/releases/latest/download/install-for-cluster.yml
export JUNCTION_ADS_SERVER="grpc://ezbake.junction.svc.cluster.local:8008"
```

## Build docker image and deploy it
```bash
docker build --tag jct_simple_app --file junction-python/samples/smoke-test/Dockerfile --load .
kubectl apply -f junction-python/samples/smoke-test/deploy/jct-simple-app.yml 
```

## Run client with static config (requires something like Orbstack to forward to k8s)
```bash
python junction-python/samples/smoke-test/client.py
```

## Run client with dynamic config
```bash
python junction-python/samples/smoke-test/client.py --use-gateway-api
```

## Run the client with dynamic config from within kube
Note the first line here is just a one off to let the client call kubectl. Further, 
this uses the global junction-python, rather than the local build. for that you need
to rebuild the docker image.
```bash
kubectl apply -f junction-python/samples/smoke-test/deploy/client-cluster-role-binding.yml
kubectl run jct-client --rm --image=jct_simple_app:latest --image-pull-policy=IfNotPresent --env="JUNCTION_ADS_SERVER=grpc://ezbake.junction.svc.cluster.local:8008" --restart=Never --attach -- python /app/client.py --use-gateway-api
```

## Clean up
```bash
kubectl delete -f junction-python/samples/smoke-test/deploy/jct-simple-app.yml  
kubectl delete -f https://github.com/junction-labs/ezbake/releases/latest/download/install-for-cluster.yml
kubectl delete -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/experimental-install.yaml
```
