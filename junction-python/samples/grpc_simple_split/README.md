# GRPC Simple Traffic split

FIXMEL Does not currently work!!

## Building server

(Once) create venv for developing with grpcio:
```bash
  python3 -m venv myvenv
  source myvenv/bin/activate
  python3 -m pip install grpcio
  python3 -m pip install grpcio_csds
  python3 -m pip install grpcio_channelz
  python3 -m pip install grpcio-tools
```

Generate protos:
```bash
  python3 -m grpc_tools.protoc -I.  --python_out=. --pyi_out=. --grpc_python_out=. ./helloworld.proto
```

## Testing with EZBake

Build server docker container:
```bash
  docker build -t xds_jct_grpc_server .
  kubectl create -f xds_jct_grpc_server.yml 
```

Run client:
```bash
python jct_http_client.py
```

Delete server:
```bash
  kubectl delete service jct-grpc-server
  kubectl delete deployment jct-grpc-server
```

## Sample using traffic director

Nothing here has been tested recently!

Nothing specific to junction/ezbake about this, just a regression test to make
sure we stay consistent between GRPC and Junction behaviours.

You'll need a GCP project and pretty wide admin permissions

### Basic setup and docker image

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

# allow the grpc clients to acquire routing data from the traffic director api
gcloud projects add-iam-policy-binding $PROJECT_ID \
    --member serviceAccount:$PROJECT_NUMBER-compute@developer.gserviceaccount.com \
    --role=roles/trafficdirector.client

# build and acquire the image's hash
pushd junction-python/samples/grpc_simple_split
docker buildx build --platform linux/arm64/v8,linux/amd64 -t us-east1-docker.pkg.dev/$PROJECT_ID/ar1/xds-td --push .
docker pull us-east1-docker.pkg.dev/$PROJECT_ID/ar1/xds-td
export IMAGE=`docker inspect us-east1-docker.pkg.dev/$PROJECT_ID/ar1/xds-td | jq -r '.[].RepoDigests[0]'`
```

### Server

```bash
# create an egress NAT for outbound traffic (we won't allow the grpc Servers to have external addresses)
gcloud compute addresses create natip --region=us-east1

gcloud compute routers create router \
    --network $NETWORK \
    --region us-east1

gcloud compute routers nats create nat-all \
  --router=router --region=us-east1 \
  --nat-external-ip-pool=natip  \
  --nat-all-subnet-ip-ranges 

# create a healthcheck on port 30163
gcloud compute health-checks create grpc echoserverhc  --grpc-service-name helloworld.Greeter --port 30163 --enable-logging

#spin up the server instances
gcloud compute  instance-templates create-with-container grpc-td --machine-type=e2-standard-2 --no-address  \
     --network $NETWORK \
     --tags=grpc-td  \
     --container-image=$IMAGE \
     --scopes=https://www.googleapis.com/auth/cloud-platform  \
     --service-account=$PROJECT_NUMBER-compute@developer.gserviceaccount.com  \
     --container-restart-policy=always

gcloud compute  instance-groups managed create grpc-ig-central \
     --base-instance-name=grpc-ig --template=grpc-td \
     --size=2 --zone=us-east1-b \
     --health-check=echoserverhc \
     --initial-delay=300

gcloud compute instance-groups set-named-ports grpc-ig-central  \
  --named-ports=grpc-echo-port:30163 \
  --zone us-east1-b

# allow the grpc healthcheck to access the server over port 30163
gcloud compute firewall-rules create grpc-vm-allow-health-checks \
   --network $NETWORK --action allow --direction INGRESS \
   --source-ranges 35.191.0.0/16,130.211.0.0/22  \
   --target-tags grpc-td \
   --rules tcp:30163
 
# create a backend service and add the incstance group
gcloud compute backend-services create grpc-echo-service \
    --global \
    --load-balancing-scheme=INTERNAL_SELF_MANAGED \
    --protocol=GRPC \
    --port-name=grpc-echo-port \
    --health-checks echoserverhc

gcloud compute backend-services add-backend grpc-echo-service \
  --instance-group grpc-ig-central \
  --instance-group-zone us-east1-b \
  --global

# create the mesh and add a route
gcloud network-services meshes import grpc-mesh --location=global << EOF
name: grpc-mesh
EOF

gcloud network-services grpc-routes import helloworld-grpc-route --location=global << EOF
name: helloworld-grpc-route
hostnames:
- helloworld-gce
meshes:
- projects/$PROJECT_ID/locations/global/meshes/grpc-mesh
rules:
- action:
    destinations:
    - serviceName: projects/$PROJECT_ID/locations/global/backendServices/grpc-echo-service
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
gcloud compute instances list
    NAME           ZONE           MACHINE_TYPE   PREEMPTIBLE  INTERNAL_IP    EXTERNAL_IP  STATUS
    grpc-ig-856t   us-east1-a  g1-small                    10.128.15.194               RUNNING
    grpc-ig-wk6m   us-east1-a  g1-small                    10.128.15.195               RUNNING
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
./td-grpc-bootstrap --config-mesh grpc-mesh --gcp-project-number=$PROJECT_NUMBER  --output=xds_bootstrap.json
export GRPC_XDS_BOOTSTRAP=`pwd`/xds_bootstrap.json
cd

git clone https://github.com/junction-labs/junction-test.git ## not public
cd junction-test/src/grpc_demo
```

First test with DNS resolution (note, the hostname for one of the servers will be different for you)
```bash
python3 ./jct_grpc_client.py dns:///grpc-ig-h090:30163 
```

Now test with the GRPC servers
```bash
python3 ./xds_client.py --adminPort 50000 xds:///helloworld-gce
```

While running, observe status 
```bash
go install -v github.com/grpc-ecosystem/grpcdebug@latest
./go/bin/grpcdebug localhost:50000 xds status
```

### Cleanup

```bash

# tear down backend and mesh
gcloud network-services grpc-routes delete helloworld-grpc-route --location=global -q
gcloud network-services meshes delete grpc-mesh -q --location=global
gcloud compute backend-services delete  grpc-echo-service --global -q


# tear down instances
gcloud compute instance-groups managed delete grpc-ig-central --zone=us-east1-b -q
gcloud compute instance-templates delete grpc-td  -q
gcloud compute firewall-rules delete grpc-vm-allow-health-checks -q --global
gcloud compute health-checks delete echoserverhc -q


# tear down NAT
gcloud compute routers nats delete nat-all --router=router --region=us-east1 -q
gcloud compute routers delete router --region=us-east1 -q
gcloud compute addresses delete natip --region=us-east1 -q
```
