use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use xds_api::pb::google::protobuf;

use crate::xds::{self, AdsClient};
use crate::{Route, RouteTarget};

/// A service discovery client that looks up URL information based on URLs,
/// headers, and methods.
///
/// Clients use a shared in-memory cache to keep data warm so that a request
/// never has to block on a remote service.
///
/// Clients are cheaply cloneable, and should be cloned to create multiple
/// clients that share the same in-memory cache.
#[derive(Clone)]
pub struct Client {
    ads: AdsClient,
    default_routes: Vec<Route>,
    _ads_task: Arc<tokio::task::JoinHandle<()>>,
}

impl Client {
    /// Build a new client, spawning a new ADS client in the background.
    ///
    /// This method creates a new ADS client and ADS connection. Data fetched
    /// over ADS won't be shared with existing clients. To create a client that
    /// shares with an existing cache, call [Client::clone] on an existing
    /// client.
    ///
    /// This function assumes that you're currently running the context of
    /// a `tokio` runtime and spawns background tasks.
    pub async fn build(
        address: String,
        node_id: String,
        cluster: String,
        default_routes: Vec<Route>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (ads, mut ads_task) = AdsClient::build(address, node_id, cluster).unwrap();

        // try to start the ADS connection while blocking. if it fails, fail
        // fast here instead of letting the client start.
        //
        // once it's started, hand off the task to the executor in the
        // background.
        ads_task.connect().await?;
        let handle = tokio::spawn(async move {
            ads_task.connect().await.expect("xds: connection failed");
            match ads_task.run().await {
                Ok(()) => (),
                Err(e) => panic!("xds: ads client exited: unxpected error: {e}"),
            }
        });

        let client = Self {
            default_routes,
            ads,
            _ads_task: Arc::new(handle),
        };
        client.subscribe_to_defaults(&client.default_routes);
        Ok(client)
    }

    pub fn with_default_routes(self, default_routes: Vec<Route>) -> Client {
        self.subscribe_to_defaults(&default_routes);
        Client {
            default_routes,
            ..self
        }
    }

    fn subscribe_to_defaults(&self, default_routes: &[Route]) {
        for route in default_routes {
            for domain in &route.domains {
                self.ads
                    .subscribe(xds::ResourceType::Listener, domain.clone())
                    .unwrap();
            }

            for rule in &route.rules {
                match &rule.target {
                    RouteTarget::Cluster(cluster_name) => self
                        .ads
                        .subscribe(xds::ResourceType::Cluster, cluster_name.clone())
                        .unwrap(),
                    RouteTarget::WeightedClusters(wcs) => {
                        for wc in wcs {
                            self.ads
                                .subscribe(xds::ResourceType::Cluster, wc.name.clone())
                                .unwrap()
                        }
                    }
                }
            }
        }
    }

    pub fn config_server(
        &self,
        port: u16,
    ) -> impl Future<Output = Result<(), tonic::transport::Error>> {
        crate::xds::csds::local_server(self.ads.cache.clone(), port)
    }

    pub fn dump(&self) -> impl Iterator<Item = (String, protobuf::Any)> + '_ {
        self.ads.cache.iter_any()
    }

    pub fn resolve_endpoints(
        &mut self,
        method: &http::Method,
        uri: http::Uri,
        headers: &http::HeaderMap,
    ) -> crate::Result<Vec<crate::Endpoint>> {
        let mut url = crate::Url::new(uri)?;

        // TODO: there's really no reasonable way to recover without starting a
        // new client. if you're on the default client (like FFI clients
        // probably are?) can you do anything about this?
        //
        // TODO: increment a metric or something if there's an error. this
        // shouldn't be a showstopper if data is already in cache.
        let _ = self
            .ads
            .subscribe(xds::ResourceType::Listener, url.hostname().to_string());

        // FIXME: this is deeply janky, manual exponential backoff. it should
        // be possible for the ADS client to signal when a cache entry is
        // available/pending a request/not found instead of just there/not.
        //
        // make that happen, and figure out if there is a way to notify when
        // that happens.
        const RESOLVE_BACKOFF: &[Duration] = &[
            Duration::from_micros(500),
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(4),
            Duration::from_millis(8),
            Duration::from_millis(8),
        ];
        for backoff in RESOLVE_BACKOFF {
            match self.get_endpoints(method, url, headers) {
                Ok(endpoints) => return Ok(endpoints),
                Err((u, e)) => {
                    if !e.is_temporary() {
                        return Err(e);
                    }

                    url = u;
                    std::thread::sleep(*backoff);
                }
            }
        }

        self.get_endpoints(method, url, headers)
            .map_err(|(_url, e)| e)
    }
}

impl Client {
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    fn get_endpoints(
        &self,
        method: &http::Method,
        url: crate::Url,
        headers: &http::HeaderMap,
    ) -> Result<Vec<crate::Endpoint>, (crate::Url, crate::Error)> {
        use rand::seq::SliceRandom;

        let xds_routes;
        let route_list = if self.default_routes.is_empty() {
            let Some(rs) = self.ads.get_routes(url.hostname()) else {
                return Err((
                    url,
                    crate::Error::NotReady {
                        reason: "no routes found",
                    },
                ));
            };
            xds_routes = rs;
            xds_routes.as_slice()
        } else {
            self.default_routes.as_slice()
        };

        let matching_route = match route_list
            .iter()
            .find_map(|vh| vh.matching_rule(method, &url, headers))
        {
            Some(rte) => rte,
            None => return Err((url, crate::Error::NoRouteMatched)),
        };

        let cluster_name = match &matching_route.target {
            RouteTarget::Cluster(name) => name,
            RouteTarget::WeightedClusters(weighted_clusters) => {
                crate::rand::with_thread_rng(|rng| {
                    &weighted_clusters
                        .choose_weighted(rng, |wc| wc.weight)
                        // TODO: this is really an invalid config error. we should have
                        // caught this earlier. for now, panic here.
                        .expect("tried to sample from an invalid config")
                        .name
                })
            }
        };

        let (lb, endpoints) = match self.ads.get_target(cluster_name) {
            (None, _) => {
                return Err((
                    url,
                    crate::Error::NotReady {
                        reason: "no load balancer configured",
                    },
                ))
            }
            (Some(_), None) => {
                return Err((
                    url,
                    crate::Error::NotReady {
                        reason: "no endpoint data found",
                    },
                ))
            }
            (Some(lb), Some(endpoints)) => (lb, endpoints),
        };

        let endpoint =
            match lb.load_balance(&url, headers, &matching_route.hash_policies, &endpoints) {
                Some(e) => e,
                None => return Err((url, crate::Error::NoReachableEndpoints)),
            };

        let timeout = matching_route.timeout.unwrap_or(Self::DEFAULT_TIMEOUT);
        let retry = matching_route.retry_policy.clone();

        Ok(vec![crate::Endpoint {
            url,
            timeout,
            retry,
            address: endpoint.clone(),
        }])
    }
}
