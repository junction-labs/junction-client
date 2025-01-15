//! Junction XDS.
//!
//! This module contains [AdsClient], the interface between Junction and XDS. A
//! client is a long-lived pile of state that gets wrapped by one or more Junction
//! Clients to present Route and Backend data to the world.
//!
//! The stateful internals of a client are written in a sans-io way as much as
//! possible to make it easier to test and verify the complexity of ADS. Most of
//! the nasty bits are wrapped up in the [cache] module - a Cache is responsible
//! for parsing and storing raw xDS and its Junction equivalent, and for tracking
//! the relationship between resources.
//!
//! An [AdsConnection] wraps a cache and a DNS resolver and multiplexes the
//! input from remote connections, subscriptions from clients and uses the
//! state of the cache to register interest in DNS names and to subscribe to
//! xDS resources.
//!
//! The [AdsTask] returned from a client is the actual io in this module - an
//! [AdsTask] actually does gRPC and listens on sockets and drives a new
//! [AdsConnection] every time it reconnects.
//!
//!  # TODO
//!
//! - Figure out how to run a Client without an upstream ADS server. Right now
//!   we don't process subscription updates until a gRPC connection gets
//!   established which seems bad.
//!
//!  - XDS client features:
//!    `envoy.lb.does_not_support_overprovisioning` and friends. See
//!    <https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md>.

use bytes::Bytes;
use delta_cache::{Cache, CacheReader};
use enum_map::EnumMap;
use futures::TryStreamExt;
use junction_api::{backend::BackendId, http::Route, Hostname, Service};
use std::{
    borrow::Cow, collections::BTreeSet, future::Future, io::ErrorKind, sync::Arc, time::Duration,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Endpoint;
use tracing::debug;
use xds_api::pb::{
    envoy::{
        config::core::v3 as xds_core,
        service::discovery::v3::{
            aggregated_discovery_service_client::AggregatedDiscoveryServiceClient,
            DeltaDiscoveryRequest, DeltaDiscoveryResponse,
        },
    },
    google::{protobuf, rpc::Status as GrpcStatus},
};

mod delta_cache;

mod resources;
pub use resources::ResourceVersion;
pub(crate) use resources::{ResourceType, ResourceVec};

use crate::{dns::StdlibResolver, BackendLb, ConfigCache};

mod csds;

#[cfg(test)]
mod test;

/// A single xDS configuration object, with additional metadata about when it
/// was fetched and processed.
#[derive(Debug, Default, Clone)]
pub struct XdsConfig {
    pub name: String,
    pub type_url: String,
    pub version: Option<ResourceVersion>,
    pub xds: Option<protobuf::Any>,
    pub last_error: Option<(ResourceVersion, String)>,
}

#[derive(Debug)]
enum SubscriptionUpdate {
    AddHosts(Vec<String>),
    AddBackends(Vec<BackendId>),

    #[allow(unused)]
    RemoveHosts(Vec<String>),
    #[allow(unused)]
    RemoveBackends(Vec<BackendId>),
}

/// A Junction ADS client that manages long-lived xDS state by connecting to a
/// remote server.
///
/// The client presents downstream as a [ConfigCache] so that a client can query
/// `Route` and `Backend` data. It also exposes a subscription interface for
/// both so that clients can register interest without having to know about the
/// details of xDS.
///
/// See the module docs for the general design of this whole module and how the
/// client pulls it all together.
#[derive(Clone)]
pub(super) struct AdsClient {
    subs: mpsc::Sender<SubscriptionUpdate>,
    cache: CacheReader,
    dns: StdlibResolver,
}

impl AdsClient {
    /// Create a new paired `AdsClient`` and `AdsTask`.
    ///
    /// A single `AdsTask` is expected to run in the background and communicate
    /// with an ADS service, while any number of `AdsClient`s can use it to read
    /// and request discovery data.
    ///
    /// This method doesn't start the background work necessary to communicate with
    /// an ADS server. To do that, call the [run][AdsTask::run] method on the returned
    /// `AdsTask`.
    pub(super) fn build(
        address: impl Into<Bytes>,
        node_id: String,
        cluster: String,
    ) -> Result<(AdsClient, AdsTask), tonic::transport::Error> {
        let endpoint = Endpoint::from_shared(address)?;

        let node_info = xds_core::Node {
            id: node_id,
            cluster,
            client_features: vec![
                "envoy.lb.does_not_support_overprovisioning".to_string(),
                "envoy.lrs.supports_send_all_clusters".to_string(),
            ],
            ..Default::default()
        };

        // TODO: how should we pick this number?
        let (sub_tx, sub_rx) = mpsc::channel(10);
        let delta_cache = Cache::default();

        // FIXME: make this configurable
        let dns = StdlibResolver::new_with(Duration::from_secs(5), Duration::from_millis(500), 2);

        let client = AdsClient {
            subs: sub_tx,
            cache: delta_cache.reader(),
            dns: dns.clone(),
        };
        let task = AdsTask {
            endpoint,
            initial_channel: None,
            node_info,
            delta_cache,
            dns,
            subs: sub_rx,
        };

        Ok((client, task))
    }

    pub(super) fn csds_server(
        &self,
        port: u16,
    ) -> impl Future<Output = Result<(), tonic::transport::Error>> + Send + 'static {
        csds::local_server(self.cache.clone(), port)
    }

    pub(super) fn iter_routes(&self) -> impl Iterator<Item = Arc<Route>> + '_ {
        self.cache.iter_routes()
    }

    pub(super) fn iter_backends(&self) -> impl Iterator<Item = Arc<BackendLb>> + '_ {
        self.cache.iter_backends()
    }

    pub(super) fn iter_xds(&self) -> impl Iterator<Item = XdsConfig> + '_ {
        self.cache.iter_xds()
    }
}

impl ConfigCache for AdsClient {
    async fn get_route<S: AsRef<str>>(&self, host: S) -> Option<Arc<Route>> {
        let hosts = vec![host.as_ref().to_string()];
        let _ = self.subs.send(SubscriptionUpdate::AddHosts(hosts)).await;

        self.cache.get_route(host).await
    }

    async fn get_backend(
        &self,
        backend: &junction_api::backend::BackendId,
    ) -> Option<std::sync::Arc<crate::BackendLb>> {
        let bs = vec![backend.clone()];
        let _ = self.subs.send(SubscriptionUpdate::AddBackends(bs)).await;

        self.cache.get_backend(backend).await
    }

    async fn get_endpoints(
        &self,
        backend: &junction_api::backend::BackendId,
    ) -> Option<std::sync::Arc<crate::EndpointGroup>> {
        let bs = vec![backend.clone()];
        let _ = self.subs.send(SubscriptionUpdate::AddBackends(bs)).await;

        match &backend.service {
            junction_api::Service::Dns(dns) => {
                self.dns
                    .get_endpoints_await(&dns.hostname, backend.port)
                    .await
            }
            _ => self.cache.get_endpoints(backend).await,
        }
    }
}

pub(crate) struct AdsTask {
    endpoint: tonic::transport::Endpoint,
    initial_channel: Option<tonic::transport::Channel>,
    node_info: xds_core::Node,
    delta_cache: Cache,
    dns: StdlibResolver,
    subs: mpsc::Receiver<SubscriptionUpdate>,
}

#[derive(Debug, thiserror::Error)]
struct ShutdownError;

impl std::fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AdsTask started after shutdown")
    }
}

macro_rules! trace_delta_request {
    ($request:expr) => {
        tracing::debug!(
            nack = $request.error_detail.is_some(),
            "DeltaDiscoveryRequest(n={:?}, ty={:?}, r={:?})",
            $request.response_nonce,
            $request.type_url,
            $request.resource_names_subscribe,
        );
    };
}

macro_rules! trace_delta_response {
    ($response:expr) => {
        tracing::debug!(
            "DeltaDiscoveryResponse(n={:?}, ty={:?}, r_count={:?})",
            $response.nonce,
            $response.type_url,
            $response.resources.len(),
        );
    };
}

impl AdsTask {
    pub(super) fn is_shutdown(&self) -> bool {
        self.subs.is_closed()
    }

    pub(super) async fn run(&mut self) -> Result<(), &(dyn std::error::Error + 'static)> {
        if self.is_shutdown() {
            return Err(&ShutdownError);
        }

        loop {
            match self.run_delta_connection().await {
                Ok(()) => break,
                // on an ADS disconnect, just reconnect
                Err(ConnectionError::AdsDisconnected) => continue,
                // On a connection error, reconnect with a backoff and try to
                // find a new ADS server.
                //
                // There's no great way to distingush between a connection
                // that's never going to work and a temporary (but long) outage,
                // so we'll just patiently keep trying.
                Err(ConnectionError::Connect(e)) => {
                    debug!(err = %e, "failed to connect to ADS server");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                // The stream closed with a Tonic error. This is usually either
                // a broken pipe or some other kind of IO error.
                //
                // There's also nothing to do here but log an error and
                // continue, but don't wait too long on broken pipe.
                Err(ConnectionError::Status(status)) => {
                    // FIXME: emit an event with tracing or metrics or something here
                    let is_broken_pipe =
                        unwrap_io_error(&status).is_some_and(|e| e.kind() == ErrorKind::BrokenPipe);

                    if !is_broken_pipe {
                        debug!(err = %status, "ADS connection closed unexpectedly");
                    }

                    tokio::time::sleep(if is_broken_pipe {
                        Duration::from_millis(50)
                    } else {
                        Duration::from_secs(2)
                    })
                    .await;
                }
            };
        }

        Ok(())
    }

    // TODO: can we split this even further from IO so we can run it without an
    // active server? it would be nice to process subscription updates even
    // while the connection is dead, and might allow adding static resources
    // directly to a cache instead of keeping a separate static cache.
    //
    // To do it in a resasonable way, we need to pull the GRPC connection out
    // of here. right now this async fn is implicitly a single-connection state
    // machine - we could keep that and have a separate disconnected loop that
    // we transition into, or we could pass a "NewConnection" message into here
    // and manually manage connected vs. disconnected state.
    async fn run_delta_connection(&mut self) -> Result<(), ConnectionError> {
        let channel = self.new_connection().await?;
        let mut client = AggregatedDiscoveryServiceClient::new(channel);

        let (xds_tx, xds_rx) = tokio::sync::mpsc::channel(10);
        let stream_response = client
            .delta_aggregated_resources(ReceiverStream::new(xds_rx))
            .await?;

        let mut incoming = stream_response.into_inner();
        let (mut conn, initial_requests) =
            DeltaAdsConnection::new(self.node_info.clone(), &mut self.delta_cache);

        for msg in initial_requests {
            trace_delta_request!(msg);
            if xds_tx.send(msg).await.is_err() {
                return Err(ConnectionError::AdsDisconnected);
            }
        }

        loop {
            tokio::select! {
                xds_msg = incoming.try_next() => {
                    // on GRPC status errors, the connection has died and we're
                    // going to reconnect. pass the error up to reset things
                    // and move on.
                    let response = match xds_msg? {
                        Some(response) => response,
                        None => return Err(ConnectionError::AdsDisconnected),
                    };
                    trace_delta_response!(response);

                    tracing::trace!("ads connection: handle_ads_message");
                    conn.handle_ads_message(response);
                }
                sub_update = self.subs.recv() => {
                    let Some(sub_update) = sub_update else {
                        return Ok(())
                    };

                    tracing::trace!(
                        ?sub_update,
                        "ads connection: handle_subscription_update",
                    );
                    conn.handle_subscription_update(sub_update);
                }
            }

            let (outgoing, dns_updates) = conn.outgoing();
            for msg in outgoing {
                trace_delta_request!(msg);
                if xds_tx.send(msg).await.is_err() {
                    return Err(ConnectionError::AdsDisconnected);
                }
            }
            update_dns(
                &mut self.dns,
                dns_updates.add,
                dns_updates.remove,
                None::<Vec<_>>,
            );
        }
    }

    pub(super) async fn connect(&mut self) -> Result<(), tonic::transport::Error> {
        if self.initial_channel.is_none() {
            let channel = self.endpoint.connect().await?;
            self.initial_channel = Some(channel)
        }

        Ok(())
    }

    async fn new_connection(
        &mut self,
    ) -> Result<tonic::transport::Channel, tonic::transport::Error> {
        match self.initial_channel.take() {
            Some(channel) => Ok(channel),
            None => self.endpoint.connect().await,
        }
    }
}

#[inline]
fn update_dns(
    dns: &StdlibResolver,
    add: BTreeSet<(Hostname, u16)>,
    remove: BTreeSet<(Hostname, u16)>,
    sync: Option<impl IntoIterator<Item = (Hostname, u16)>>,
) {
    for (name, port) in add {
        dns.subscribe(name, port);
    }
    for (name, port) in remove {
        dns.unsubscribe(&name, port);
    }

    if let Some(names) = sync {
        dns.set_names(names);
    }
}

#[derive(Debug, thiserror::Error)]
enum ConnectionError {
    #[error(transparent)]
    Connect(#[from] tonic::transport::Error),

    #[error(transparent)]
    Status(#[from] tonic::Status),

    #[error("ADS server closed the stream")]
    AdsDisconnected,
}

/// Returns `true` if this tonic [Status] was caused by a [std::io::Error].
///
/// Adapted from the `tonic` examples.
///
/// https://github.com/hyperium/tonic/blob/941726cc46b995dcc393c9d2b462d440bd3514f3/examples/src/streaming/server.rs#L15
fn unwrap_io_error(status: &tonic::Status) -> Option<&std::io::Error> {
    let mut err: &(dyn std::error::Error + 'static) = status;

    loop {
        if let Some(e) = err.downcast_ref::<std::io::Error>() {
            return Some(e);
        }

        // https://github.com/hyperium/h2/pull/462
        if let Some(e) = err.downcast_ref::<h2::Error>().and_then(|e| e.get_io()) {
            return Some(e);
        }

        err = err.source()?;
    }
}

struct DeltaAdsConnection<'a> {
    cache: &'a mut Cache,
    node: Option<xds_core::Node>,
    acks: EnumMap<ResourceType, Option<AckState>>,
    unknown_types: Vec<(String, String)>,
}

#[derive(Debug, Default)]
struct AckState {
    nonce: String,
    error: Option<Cow<'static, str>>,
}

impl AckState {
    fn into_ack(self) -> (String, Option<GrpcStatus>) {
        let nonce = self.nonce;
        let error = self.error.map(|message| GrpcStatus {
            message: message.to_string(),
            code: tonic::Code::InvalidArgument.into(),
            ..Default::default()
        });

        (nonce, error)
    }
}

impl<'a> DeltaAdsConnection<'a> {
    fn new(node: xds_core::Node, cache: &'a mut Cache) -> (Self, Vec<DeltaDiscoveryRequest>) {
        let mut requests = Vec::with_capacity(ResourceType::all().len());

        let mut node = Some(node);
        for &rtype in ResourceType::all() {
            let initial_versions = cache.versions(rtype);
            let mut subscribe = cache.initial_subscriptions(rtype);
            if cache.is_wildcard(rtype) && !subscribe.is_empty() {
                subscribe.push("*".to_string());
            }

            if !cache.is_wildcard(rtype) && subscribe.is_empty() && initial_versions.is_empty() {
                continue;
            }

            requests.push(DeltaDiscoveryRequest {
                node: node.take(),
                type_url: rtype.type_url().to_string(),
                resource_names_subscribe: subscribe,
                initial_resource_versions: initial_versions,
                ..Default::default()
            });
        }

        let conn = Self {
            cache,
            node,
            acks: Default::default(),
            unknown_types: Vec::new(),
        };
        (conn, requests)
    }

    fn outgoing(&mut self) -> (Vec<DeltaDiscoveryRequest>, DnsUpdates) {
        // map changes into responses. DNS updates get passed through (woo)
        let (resources, dns) = self.cache.changes();

        // EnumMap::into_iter will always cover all variants as keys in xDS
        // make-before-break order.
        let mut responses = Vec::with_capacity(ResourceType::all().len());
        for (rtype, changes) in resources {
            let ack = self.get_ack(rtype);

            if ack.is_none() && changes.is_empty() {
                continue;
            }

            let node = self.node.take();
            let (response_nonce, error_detail) = ack.map(|a| a.into_ack()).unwrap_or_default();
            let resource_names_subscribe = changes.added.into_iter().collect();
            let resource_names_unsubscribe = changes.removed.into_iter().collect();

            responses.push(DeltaDiscoveryRequest {
                node,
                type_url: rtype.type_url().to_string(),
                response_nonce,
                error_detail,
                resource_names_subscribe,
                resource_names_unsubscribe,
                ..Default::default()
            })
        }

        // hyper rare ocurrence, take the vec to reset the allocation
        for (response_nonce, type_url) in std::mem::take(&mut self.unknown_types) {
            responses.push(DeltaDiscoveryRequest {
                type_url,
                response_nonce,
                ..Default::default()
            })
        }

        (responses, dns)
    }

    fn handle_ads_message(&mut self, resp: DeltaDiscoveryResponse) {
        let Some(rtype) = ResourceType::from_type_url(&resp.type_url) else {
            tracing::trace!(type_url = %resp.type_url, "unknown type url");
            self.set_unknown(resp.nonce, resp.type_url);
            return;
        };

        // add resources
        let resources = match ResourceVec::from_resources(rtype, resp.resources) {
            Ok(r) => r,
            Err(e) => {
                tracing::trace!(err = %e, "invalid proto");
                self.set_ack(rtype, resp.nonce, Some("invalid resource".into()));
                return;
            }
        };

        let resource_errors = self.cache.insert(resources);
        let error = match &resource_errors[..] {
            &[] => None,
            // TOOD: actually generate a useful error message here
            _ => Some("invalid resources".into()),
        };
        self.set_ack(rtype, resp.nonce, error);

        // remove resources
        self.cache.remove(rtype, &resp.removed_resources);
    }

    fn handle_subscription_update(&mut self, update: SubscriptionUpdate) {
        match update {
            SubscriptionUpdate::AddHosts(hosts) => {
                for host in hosts {
                    self.cache.subscribe(ResourceType::Listener, &host);
                }
            }
            SubscriptionUpdate::RemoveHosts(hosts) => {
                for host in hosts {
                    self.cache.unsubscribe(ResourceType::Listener, &host);
                }
            }
            SubscriptionUpdate::AddBackends(backends) => {
                for backend in backends {
                    self.cache.subscribe(ResourceType::Cluster, &backend.name());

                    if let Service::Dns(dns) = &backend.service {
                        self.cache.subscribe_dns(dns.hostname.clone(), backend.port);
                    }
                }
            }
            SubscriptionUpdate::RemoveBackends(backends) => {
                for backend in backends {
                    self.cache
                        .unsubscribe(ResourceType::Cluster, &backend.name());

                    if let Service::Dns(dns) = &backend.service {
                        self.cache
                            .unsubscribe_dns(dns.hostname.clone(), backend.port);
                    }
                }
            }
        }
    }

    fn set_unknown(&mut self, nonce: String, type_url: String) {
        self.unknown_types.push((nonce, type_url))
    }

    fn set_ack(&mut self, rtype: ResourceType, nonce: String, error: Option<Cow<'static, str>>) {
        self.acks[rtype] = Some(AckState { nonce, error })
    }

    fn get_ack(&mut self, rtype: ResourceType) -> Option<AckState> {
        self.acks[rtype].take()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct DnsUpdates {
    add: BTreeSet<(Hostname, u16)>,
    remove: BTreeSet<(Hostname, u16)>,
    sync: bool,
}

#[cfg(test)]
impl DnsUpdates {
    fn is_noop(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty() && !self.sync
    }
}

#[cfg(test)]
mod test_ads_conn {
    use delta_cache::Cache;
    use once_cell::sync::Lazy;
    use pretty_assertions::assert_eq;

    use super::test as xds_test;
    use super::*;

    static TEST_NODE: Lazy<xds_core::Node> = Lazy::new(|| xds_core::Node {
        id: "unit-test".to_string(),
        ..Default::default()
    });

    /// create a new connection with TEST_NODE and the given cache. asserts that
    /// the first outgoing message has its Node set to TEST_NODE.
    #[track_caller]
    fn new_conn(cache: &mut Cache) -> (DeltaAdsConnection, Vec<DeltaDiscoveryRequest>) {
        let (conn, mut outgoing) = DeltaAdsConnection::new(TEST_NODE.clone(), cache);

        // assert the node is there
        if let Some(first) = outgoing.first_mut() {
            let node = first
                .node
                .take()
                .expect("expected first outgoing request to have a node");

            assert_eq!(node, *TEST_NODE);
        };

        (conn, outgoing)
    }

    #[test]
    fn test_init_empty_wildcard() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Listener, true);
        cache.set_wildcard(ResourceType::Cluster, true);

        let (_, outgoing) = new_conn(&mut cache);

        assert_eq!(
            outgoing,
            vec![
                xds_test::req!(t = ResourceType::Cluster),
                xds_test::req!(t = ResourceType::Listener),
            ]
        )
    }

    #[test]
    fn test_init_empty_explicit() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Listener, false);
        cache.set_wildcard(ResourceType::Cluster, false);

        let (_, outgoing) = new_conn(&mut cache);
        assert!(outgoing.is_empty());
    }

    #[test]
    fn test_init_subscription_wildcard() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Listener, false);
        cache.set_wildcard(ResourceType::Cluster, true);

        cache.subscribe(ResourceType::Cluster, "cluster.example:7891");
        cache.subscribe(ResourceType::ClusterLoadAssignment, "cluster.example:7891");

        // only the Clusters should have the wildcard sub, CLA should not, since it's
        // not a wildcard-capable resource type
        let (_, outgoing) = new_conn(&mut cache);
        assert_eq!(
            outgoing,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    add = vec!["cluster.example:7891", "*"],
                    init = vec![],
                ),
                xds_test::req!(
                    t = ResourceType::ClusterLoadAssignment,
                    add = vec!["cluster.example:7891",],
                    init = vec![],
                )
            ]
        );
    }

    #[test]
    fn test_init_subscription_explicit() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Listener, false);
        cache.set_wildcard(ResourceType::Cluster, false);

        cache.subscribe(ResourceType::Cluster, "cluster.example:7891");
        cache.subscribe(ResourceType::ClusterLoadAssignment, "cluster.example:7891");

        let (_, outgoing) = new_conn(&mut cache);
        assert_eq!(
            outgoing,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    add = vec!["cluster.example:7891",],
                    init = vec![],
                ),
                xds_test::req!(
                    t = ResourceType::ClusterLoadAssignment,
                    add = vec!["cluster.example:7891",],
                    init = vec![],
                ),
            ]
        );
    }

    #[test]
    fn test_init_initial_versions() {
        let mut cache = Cache::default();
        assert!(cache.is_wildcard(ResourceType::Listener));
        assert!(!cache.is_wildcard(ResourceType::RouteConfiguration));

        cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!("cooler.example.org", "cool-route")],
        ));
        cache.insert(ResourceVec::from_listeners(
            "456".into(),
            vec![xds_test::listener!("warmer.example.org", "warm-route")],
        ));
        cache.insert(ResourceVec::from_route_configs(
            "789".into(),
            vec![xds_test::route_config!(
                "cool-route",
                vec![xds_test::vhost!(
                    "an-vhost",
                    ["cooler.example.org"],
                    [xds_test::route!(default "cooler.example.internal:8008")]
                )]
            )],
        ));

        // both wildcard and non-wildcard should start with an empty add list
        // but resources in init
        let (_, outgoing) = new_conn(&mut cache);
        assert_eq!(
            outgoing,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    add = vec!["cooler.example.internal:8008", "*"],
                    init = vec![],
                ),
                xds_test::req!(
                    t = ResourceType::Listener,
                    add = vec![],
                    init = vec![("cooler.example.org", "123"), ("warmer.example.org", "456"),]
                ),
                xds_test::req!(
                    t = ResourceType::RouteConfiguration,
                    add = vec!["warm-route"],
                    init = vec![("cool-route", "789")]
                ),
            ],
        );
    }

    #[test]
    fn test_handle_subscribe_hostname() {
        let mut cache = Cache::default();
        let (mut conn, _) = new_conn(&mut cache);

        conn.handle_subscription_update(SubscriptionUpdate::AddHosts(vec![
            Service::dns("website.internal").unwrap().name(),
            Service::kube("default", "nginx")
                .unwrap()
                .as_backend_id(4443)
                .name(),
        ]));

        let (outgoing, dns) = conn.outgoing();
        // dns should not update on listeners
        assert!(dns.is_noop());
        assert_eq!(
            outgoing,
            vec![xds_test::req!(
                t = ResourceType::Listener,
                add = vec!["nginx.default.svc.cluster.local:4443", "website.internal"],
            )]
        );
    }

    #[test]
    fn test_handle_subscribe_backend() {
        let mut cache = Cache::default();
        let (mut conn, _) = new_conn(&mut cache);

        conn.handle_subscription_update(SubscriptionUpdate::AddBackends(vec![
            Service::dns("website.internal").unwrap().as_backend_id(80),
            Service::kube("default", "nginx")
                .unwrap()
                .as_backend_id(4443),
        ]));

        let (outgoing, dns) = conn.outgoing();
        // dns should preemptively update on dns backends
        assert_eq!(
            dns,
            DnsUpdates {
                add: [(Hostname::from_static("website.internal"), 80)]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
        );

        // should generate xds for clusters
        assert_eq!(
            outgoing,
            vec![xds_test::req!(
                t = ResourceType::Cluster,
                add = vec![
                    "nginx.default.svc.cluster.local:4443",
                    "website.internal:80"
                ],
            )]
        );
    }

    #[test]
    fn test_handle_ads_message_listener_route() {
        let mut cache = Cache::default();
        assert!(cache.is_wildcard(ResourceType::Listener));

        let (mut conn, _) = new_conn(&mut cache);

        conn.handle_ads_message(xds_test::resp!(
            n = "1",
            add = ResourceVec::from_listeners(
                "123".into(),
                vec![xds_test::listener!("cooler.example.org", "cool-route")],
            ),
            remove = vec![],
        ));
        conn.handle_ads_message(xds_test::resp!(
            n = "2",
            add = ResourceVec::from_listeners(
                "456".into(),
                vec![xds_test::listener!("warmer.example.org", "warm-route")],
            ),
            remove = vec![],
        ));
        conn.handle_ads_message(xds_test::resp!(
            n = "3",
            add = ResourceVec::from_route_configs(
                "789".into(),
                vec![xds_test::route_config!(
                    "cool-route",
                    vec![xds_test::vhost!(
                        "an-vhost",
                        ["cooler.example.org"],
                        [xds_test::route!(default "cooler.example.internal:8008")]
                    )]
                )],
            ),
            remove = vec![],
        ));

        let (outgoing, dns) = conn.outgoing();
        // no dns changes until we get a cluster
        assert!(dns.is_noop());

        assert_eq!(
            outgoing,
            vec![
                // new resource subs
                xds_test::req!(
                    t = ResourceType::Cluster,
                    add = vec!["cooler.example.internal:8008"]
                ),
                // listener ack
                xds_test::req!(t = ResourceType::Listener, n = "2"),
                // route config acks and new sub
                xds_test::req!(
                    t = ResourceType::RouteConfiguration,
                    n = "3",
                    add = vec!["warm-route"]
                ),
            ],
        );
    }

    #[test]
    fn test_handle_ads_message_cluster_cla() {
        let mut cache = Cache::default();
        assert!(cache.is_wildcard(ResourceType::Cluster));

        let (mut conn, _) = new_conn(&mut cache);

        conn.handle_ads_message(xds_test::resp!(
            n = "1",
            add = ResourceVec::from_clusters(
                "123".into(),
                vec![
                    xds_test::cluster!("cooler.example.org:2345"),
                    xds_test::cluster!("thing.default.svc.cluster.local:9876"),
                ],
            ),
            remove = vec![],
        ));
        conn.handle_ads_message(xds_test::resp!(
            n = "2",
            add = ResourceVec::from_load_assignments(
                "123".into(),
                vec![xds_test::cla!(
                    "thing.default.svc.cluster.local:9876" => {
                        "zone1" => ["1.1.1.1"]
                    }
                )],
            ),
            remove = vec![],
        ));
        conn.handle_ads_message(xds_test::resp!(
            n = "3",
            add = ResourceVec::from_listeners("555".into(), vec![
                xds_test::listener!("cooler.example.org.lb.jct:2345", "lb-route" => [xds_test::vhost!(
                    "lb-vhost",
                    ["cooler.example.org.lb.jct:2345"],
                    [xds_test::route!(default ring_hash = "x-user", "cooler.example.org:2345")],
                )]),
                xds_test::listener!("thing.default.svc.cluster.local.lb.jct:9876", "lb-route" => [xds_test::vhost!(
                    "lb-vhost",
                    ["cooler.example.org.lb.jct:2345"],
                    [xds_test::route!(default ring_hash = "x-user", "thing.default.svc.cluster.local:9876")],
                )])
            ]),
            remove = vec![],
        ));

        let (outgoing, dns) = conn.outgoing();
        // dns changes, we got a dns cluster
        assert_eq!(
            dns,
            DnsUpdates {
                add: [(Hostname::from_static("cooler.example.org"), 2345)]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
        );
        // should generate ACKs
        assert_eq!(
            outgoing,
            vec![
                xds_test::req!(t = ResourceType::Cluster, n = "1"),
                xds_test::req!(t = ResourceType::ClusterLoadAssignment, n = "2"),
                xds_test::req!(t = ResourceType::Listener, n = "3"),
            ]
        );
    }

    #[test]
    fn test_set_node_after_init() {
        let mut cache = Cache::default();
        for rtype in ResourceType::all() {
            cache.set_wildcard(*rtype, false);
        }

        let (mut conn, outgoing) = new_conn(&mut cache);
        assert!(outgoing.is_empty());

        let svc = Service::dns("website.internal").unwrap().as_backend_id(80);
        conn.handle_subscription_update(SubscriptionUpdate::AddBackends(vec![svc]));

        let (outgoing, _) = conn.outgoing();
        assert_eq!(outgoing[0].node.as_ref(), Some(&*TEST_NODE));
    }
}
