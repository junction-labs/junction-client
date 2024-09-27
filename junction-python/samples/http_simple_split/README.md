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

## Using junction-client with Google Traffic Director

FIXME: None of this has been tested recently!!

You'll need a GCP project and pretty wide admin permissions

### One off basic setup

```bash
# set some env vars. note zone is set to us-east-b in a bunch of places below.
export NETWORK=default
export PROJECT_ID=`gcloud config get-value core/project`
export PROJECT_NUMBER=`gcloud projects describe $PROJECT_ID --format='value(projectNumber)'`

# enable services; we will save the image we build on artifact registry
gcloud services enable  \
     artifactregistry.googleapis.com  \
     iam.googleapis.com trafficdirector.googleapis.com compute.googleapis.com

# create a registry and allow the default svc account for the servers to pull and run the image
gcloud artifacts repositories create ar1 --repository-format=docker --location=us-east1

gcloud artifacts repositories add-iam-policy-binding ar1 --location=us-east1  \
    --member=serviceAccount:$PROJECT_NUMBER-compute@developer.gserviceaccount.com  \
     --role=roles/artifactregistry.reader

# allow the clients to acquire routing data from the traffic director api
gcloud projects add-iam-policy-binding $PROJECT_ID \
    --member serviceAccount:$PROJECT_NUMBER-compute@developer.gserviceaccount.com \
    --role=roles/trafficdirector.client

gcloud compute addresses create natip --region=us-east1

gcloud compute routers create router \
    --network $NETWORK \
    --region us-east1

gcloud compute routers nats create nat-all \
  --router=router --region=us-east1 \
  --nat-external-ip-pool=natip  \
  --nat-all-subnet-ip-ranges 
```

### create docker image

```bash
# build and acquire the image's hash
docker buildx build --platform linux/arm64/v8,linux/amd64 -t us-east1-docker.pkg.dev/$PROJECT_ID/ar1/jct-http-server --push .
docker pull us-east1-docker.pkg.dev/$PROJECT_ID/ar1/jct-http-server
export IMAGE=`docker inspect us-east1-docker.pkg.dev/$PROJECT_ID/ar1/jct-http-server | jq -r '.[].RepoDigests[0]'`
```

### Server

```bash
# create a healthcheck
gcloud compute health-checks create http jct-http-server-hc --port 30163 --enable-logging

#spin up the server instances
gcloud compute  instance-templates create-with-container jct-http-server --machine-type=e2-standard-2 --no-address  \
     --network $NETWORK \
     --tags=jct-http-server  \
     --container-image=$IMAGE \
     --scopes=https://www.googleapis.com/auth/cloud-platform  \
     --service-account=$PROJECT_NUMBER-compute@developer.gserviceaccount.com  \
     --container-restart-policy=always

gcloud compute  instance-groups managed create jct-http-server-ig \
     --base-instance-name=jct-http-server-ig --template=jct-http-server \
     --size=2 --zone=us-east1-b \
     --health-check=jct-http-server-hc \
     --initial-delay=300

gcloud compute instance-groups set-named-ports jct-http-server-ig  \
  --named-ports=jct-http-server-port:30163 \
  --zone us-east1-b

# allow the healthcheck to access the server over port 30163
gcloud compute firewall-rules create jct-http-server-vm-allow-health-checks \
   --network $NETWORK --action allow --direction INGRESS \
   --source-ranges 35.191.0.0/16,130.211.0.0/22  \
   --target-tags jct-http-server \
   --rules tcp:30163
 
# create a backend service and add the incstance group
gcloud compute backend-services create jct-http-server-service \
    --global \
    --load-balancing-scheme=INTERNAL_SELF_MANAGED \
    --protocol=HTTP \
    --port-name=jct-http-server-port \
    --health-checks jct-http-server-hc

gcloud compute backend-services add-backend jct-http-server-service \
  --instance-group jct-http-server-ig \
  --instance-group-zone us-east1-b \
  --global


# create the mesh and add a route
gcloud network-services meshes import jct-http-server-mesh --location=global << EOF
name: jct-http-server-mesh
EOF

gcloud network-services http-routes import jct-http-server-http-route --location=global << EOF
name: jct-http-server-http-route
hostnames:
- jct-http-server-gce
meshes:
- projects/$PROJECT_ID/locations/global/meshes/jct-http-server-mesh
rules:
- action:
    destinations:
    - serviceName: projects/$PROJECT_ID/locations/global/backendServices/jct-http-server-service
EOF
```
### Client

Run it

```bash
 gcloud compute instances create xds-client      --image-family=debian-11  \
      --network $NETWORK   --machine-type=e2-standard-2 \
      --image-project=debian-cloud    \
      --scopes=https://www.googleapis.com/auth/cloud-platform  --zone=us-east1-b \
      --service-account=$PROJECT_NUMBER-compute@developer.gserviceaccount.com  
```

in my case the instances we have now are

```bash
$ gcloud compute instances list
    NAME           ZONE           MACHINE_TYPE   PREEMPTIBLE  INTERNAL_IP    EXTERNAL_IP  STATUS
    jct-http-server-ig-856t   us-east1-a  g1-small                    10.128.15.194               RUNNING
    jct-http-server-ig-wk6m   us-east1-a  g1-small                    10.128.15.195               RUNNING
    xds-client     us-east1-a  n1-standard-1               10.128.15.197  34.71.72.77  RUNNING
```

SSH to xds-client vm and set it up

```bash
gcloud compute ssh xds-client --zone  us-east1-b

# set up
sudo apt-get update && sudo apt-get install wget zip git pip -y
python3 -m pip install grpcio
python3 -m pip install grpcio-tools
python3 -m pip install grpcio_csds
python3 -m pip install grpcio_channelz
wget https://golang.org/dl/go1.20.1.linux-amd64.tar.gz
sudo tar -C /usr/local -xzf go1.20.1.linux-amd64.tar.gz
export PATH=$PATH:/usr/local/go/bin
```

Configure the bootstrap file:

```bash
git clone https://github.com/GoogleCloudPlatform/traffic-director-grpc-bootstrap.git
cd traffic-director-grpc-bootstrap/
go build .
export PROJECT_NUMBER=`curl -s "http://metadata.google.internal/computeMetadata/v1/project/numeric-project-id" -H "Metadata-Flavor: Google"`
./td-grpc-bootstrap --config-mesh-experimental --gcp-project-number=$PROJECT_NUMBER  --output=xds_bootstrap.json
export GRPC_XDS_BOOTSTRAP=`pwd`/xds_bootstrap.json
cd

git clone https://github.com/junction-labs/junction-test.git ## not public
cd junction-test/src/grpc_demo
```

First test with DNS resolution (note, the hostname for one of the servers will be different for you)
```bash
python3 ./jct_grpc_client.py dns:///jct-http-server-ig-h090:30163 
```

Now test with the GRPC servers
```bash
python3 ./xds_client.py --adminPort 50000 xds:///jct-http-server-gce
```

### Cleanup

```bash

# tear down backend and mesh
gcloud network-services http-routes delete jct-http-server-http-route --location=global -q
gcloud network-services meshes delete jct-http-server-mesh --location=global -q
gcloud compute backend-services delete jct-http-server-service --global -q

# tear down instances
gcloud compute instance-groups managed delete jct-http-server-ig --zone=us-east1-b -q
gcloud compute instance-templates delete jct-http-server  -q
gcloud compute firewall-rules delete jct-http-server-vm-allow-health-checks -q
gcloud compute health-checks delete jct-http-server-hc -q

# tear down NAT
gcloud compute routers nats delete nat-all --router=router --region=us-east1 -q
gcloud compute routers delete router --region=us-east1 -q
gcloud compute addresses delete natip --region=us-east1 -q
```
