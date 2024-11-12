// A sans-io implementation of an SotW ADS client.
//
// This module supports a subset of XDS that should mirror GRPCs as closely
// as possible, diverging where it makes sense for HTTP protocol support.
//
// The the client is built around a cache of both XDS and internal config. The
// single connection handling thread owns the cache, but any number of readers
// can read from it concurrently. See the cache module for more.
//
// As XDS is read from the wire, it's validated and transformed into internal
// configuration structs. See the `resources` module for all of the gory details
// about what's considered valid and what isn't.
//
//
// # TODO
//
// - Support XDS client features like
//   `envoy.lb.does_not_support_overprovisioning` and friends. See
//   https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md.
//
// - Support for HTTP filters and per-route filter overrides.
//   https://github.com/grpc/proposal/blob/master/A39-xds-http-filters.md
//
// - XDS config dumps, potentially through CSRS.
//
//- Support the Envoy LRS protocol for sending load back to the control plane.
//  https://www.envoyproxy.io/docs/envoy/latest/start/sandboxes/load-reporting-service.html

use bytes::Bytes;
use cache::{Cache, CacheReader};
use enum_map::EnumMap;
use futures::TryStreamExt;
use std::{
    collections::BTreeSet,
    io::ErrorKind,
    time::{Duration, Instant},
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
            DiscoveryRequest, DiscoveryResponse,
        },
    },
    google::rpc::Status as GrpcStatus,
};

mod cache;
pub use cache::XdsConfig;

mod resources;
pub use resources::ResourceVersion;
pub(crate) use resources::{ResourceType, ResourceVec};

pub(crate) mod csds;

#[cfg(test)]
mod test;

#[derive(Clone)]
pub struct AdsClient {
    subscriptions: mpsc::Sender<SubscriptionUpdate>,
    pub(crate) cache: CacheReader,
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
    pub fn build(
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
        let cache = Cache::default();

        let client = AdsClient {
            subscriptions: sub_tx,
            cache: cache.reader(),
        };
        let task = AdsTask {
            endpoint,
            initial_channel: None,
            node_info,
            cache,
            subscriptions: sub_rx,
        };

        Ok((client, task))
    }

    // FIXME: add a timeout or let the caller do it?
    pub async fn subscribe(
        &self,
        resource_type: ResourceType,
        name: String,
    ) -> Result<notify::Recv, ()> {
        let (update, recv) = SubscriptionUpdate::add(resource_type, name);

        self.subscriptions.send(update).await.map_err(|_| ())?;

        Ok(recv)
    }
}

pub(crate) struct AdsTask {
    endpoint: tonic::transport::Endpoint,
    initial_channel: Option<tonic::transport::Channel>,
    node_info: xds_core::Node,
    cache: Cache,
    subscriptions: mpsc::Receiver<SubscriptionUpdate>,
}

#[derive(Debug, thiserror::Error)]
struct ShutdownError;

impl std::fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AdsTask started after shutdown")
    }
}

macro_rules! trace_xds_request {
    ($request:expr) => {
        tracing::trace!(
            nack = $request.error_detail.is_some(),
            "DiscoveryRequest(v={:?}, n={:?}, ty={:?}, r={:?})",
            $request.version_info,
            $request.response_nonce,
            $request.type_url,
            $request.resource_names,
        );
    };
}

macro_rules! trace_xds_response {
    ($response:expr) => {
        tracing::trace!(
            "DiscoveryResponse(v={:?}, n={:?}, ty={:?}, r_count={:?})",
            $response.version_info,
            $response.nonce,
            $response.type_url,
            $response.resources.len(),
        );
    };
}

impl AdsTask {
    pub fn is_shutdown(&self) -> bool {
        self.subscriptions.is_closed()
    }

    pub async fn run(&mut self) -> Result<(), &(dyn std::error::Error + 'static)> {
        if self.is_shutdown() {
            return Err(&ShutdownError);
        }

        loop {
            match self.run_connection().await {
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
                    let is_broken_pipe = unwrap_io_error(&status)
                        .map_or(false, |e| e.kind() == ErrorKind::BrokenPipe);

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

    /// Run a single ADS connection to completion. Returns `Ok(())` if the
    /// connection exited cleanly, or an error otherwise.
    async fn run_connection(&mut self) -> Result<(), ConnectionError> {
        // TODO: this should be configurable in Client. leave it as a const here
        // for now, but it needs to be exposed sooner or later.
        const TIMEOUT: Duration = Duration::from_secs(5);

        let channel = self.new_connection().await?;
        let mut client = AggregatedDiscoveryServiceClient::new(channel);

        let (xds_tx, xds_rx) = tokio::sync::mpsc::channel(10);
        let stream_response = client
            .stream_aggregated_resources(ReceiverStream::new(xds_rx))
            .await?;

        let mut node = Some(self.node_info.clone());
        let mut incoming = stream_response.into_inner();
        let mut timeouts = BTreeSet::new();

        let (mut conn, outgoing) = AdsConnection::new(&mut self.cache);

        let should_set_timeout = !outgoing.is_empty();
        for mut msg in outgoing {
            if let Some(node) = node.take() {
                msg.node = Some(node)
            }
            trace_xds_request!(&msg);
            xds_tx.send(msg).await.unwrap();
        }
        if should_set_timeout {
            timeouts.insert(Instant::now() + TIMEOUT);
        }

        loop {
            let timeout = timeouts.first();
            let outgoing = tokio::select! {
                _ = sleep_until(timeout.cloned()) => {
                    // when a timeout fires notify the connection and drop all
                    // of the timeouts that are smaller than now. there's no
                    // reason to fire timeouts mutliple times.
                    let now = Instant::now();
                    conn.handle_timeout(now, TIMEOUT);
                    prune_timeouts(&mut timeouts, now);
                    vec![]
                }
                xds_msg = incoming.try_next() => {
                    // on GRPC status errors, the connection has died and we're
                    // going to reconnect. pass the error up to reset things
                    // and move on.
                    let response = match xds_msg? {
                        Some(response) => response,
                        None => return Err(ConnectionError::AdsDisconnected),
                    };
                    trace_xds_response!(response);

                    // on XDS errors, just say fuck it and return so that the
                    // connection resets. there's something fucked up that will
                    // be fixed best by resetting connection state.
                    conn.handle_ads_message(response)
                }
                sub_update = self.subscriptions.recv() => {
                    match sub_update {
                        Some(update) => {
                            let updates = conn.handle_subscription_update(Instant::now(), update);
                            updates.into_iter().collect()
                        },
                        None => return Ok(()),
                    }
                }
            };

            let should_set_timeout = !outgoing.is_empty();
            for mut msg in outgoing {
                if let Some(node) = node.take() {
                    msg.node = Some(node)
                }
                trace_xds_request!(msg);
                xds_tx.send(msg).await.unwrap();
            }
            if should_set_timeout {
                timeouts.insert(Instant::now() + TIMEOUT);
            }
        }
    }

    pub async fn connect(&mut self) -> Result<(), tonic::transport::Error> {
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

/// drop early timeouts from the front of the set. this is potentially
/// quicker than retain, since it relies on the fact that the set
/// is ordered to avoid walking the whole thing.
fn prune_timeouts(timeouts: &mut BTreeSet<Instant>, now: Instant) {
    loop {
        match timeouts.first() {
            Some(t) if *t <= now => (),
            _ => break,
        }

        timeouts.pop_first();
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => futures::future::pending().await,
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

/// Single-use notifications, fire and forget.
pub(crate) mod notify {
    use futures::FutureExt;

    /// Create a single-use sender and receiver pair.
    pub(crate) fn new() -> (Send, Recv) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (Send(tx), Recv(rx))
    }

    /// The send half.
    #[derive(Debug)]
    pub(crate) struct Send(tokio::sync::oneshot::Sender<()>);

    impl Send {
        /// Send a notification. Always succeeds, even if the receiver is
        /// already gone.
        pub(crate) fn notify(self) {
            let _ = self.0.send(());
        }
    }

    /// The recv half.
    #[derive(Debug)]
    pub(crate) struct Recv(tokio::sync::oneshot::Receiver<()>);

    impl Recv {
        /// Wait for a notification. Returns an Error if the sender dropped
        /// before sending.
        pub(crate) fn wait(self) -> impl std::future::Future<Output = ()> + Unpin {
            self.0.map(|_| ())
        }

        #[cfg(test)]
        pub(crate) fn notified(&mut self) -> bool {
            matches!(self.0.try_recv(), Ok(()))
        }

        #[cfg(test)]
        pub(crate) fn pending(&mut self) -> bool {
            matches!(
                self.0.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            )
        }
    }
}

#[derive(Debug)]
struct SubscriptionUpdate {
    notify: notify::Send,
    update: SubscriptionUpdateType,
}

impl SubscriptionUpdate {
    fn add(resource_type: ResourceType, name: String) -> (Self, notify::Recv) {
        let (send, recv) = notify::new();
        let update = Self {
            notify: send,
            update: SubscriptionUpdateType::Add(resource_type, name),
        };

        (update, recv)
    }
}

#[derive(Debug)]
enum SubscriptionUpdateType {
    Add(ResourceType, String),

    #[allow(unused)]
    Remove(ResourceType, String),
}

#[derive(Debug)]
struct AdsConnection<'a> {
    cache: &'a mut Cache,
    last_nonce: EnumMap<ResourceType, Option<String>>,
    resource_versions: EnumMap<ResourceType, Option<String>>,
}

impl<'a> AdsConnection<'a> {
    fn new(cache: &'a mut Cache) -> (Self, Vec<DiscoveryRequest>) {
        let conn = Self {
            last_nonce: EnumMap::default(),
            resource_versions: EnumMap::default(),
            cache,
        };

        let mut outgoing = Vec::with_capacity(4);
        for resource_type in ResourceType::all() {
            let msg = conn.xds_subscription(*resource_type);
            if !msg.resource_names.is_empty() {
                outgoing.push(msg);
            }
        }
        (conn, outgoing)
    }
}

impl<'a> AdsConnection<'a> {
    fn handle_subscription_update(
        &mut self,
        now: Instant,
        update: SubscriptionUpdate,
    ) -> Option<DiscoveryRequest> {
        let (rtype, changed) = match update.update {
            SubscriptionUpdateType::Add(rtype, name) => {
                (rtype, self.cache.subscribe(rtype, name, now, update.notify))
            }
            SubscriptionUpdateType::Remove(rtype, name) => (rtype, self.cache.delete(rtype, &name)),
        };

        changed.then(|| self.xds_subscription(rtype))
    }

    fn handle_timeout(&mut self, now: Instant, timeout: Duration) {
        self.cache.handle_timeouts(now, timeout);
    }

    fn handle_ads_message(
        &mut self,
        discovery_response: DiscoveryResponse,
    ) -> Vec<DiscoveryRequest> {
        // On an unrecognized type, instantly NACK.
        let Some(resource_type) = ResourceType::from_type_url(&discovery_response.type_url) else {
            tracing::trace!(
                type_url = &discovery_response.type_url,
                "unknown resource type"
            );
            return vec![xds_unknown_type(discovery_response)];
        };

        // parse protos from Any, for the first time, and move the version/nonce
        // of the request out of the incoming message.
        let (version, nonce) = (discovery_response.version_info, discovery_response.nonce);
        let resources = match ResourceVec::from_any(resource_type, discovery_response.resources) {
            Ok(r) => r,
            Err(e) => {
                let error_detail = format!("protobuf decoding error: '{}'", e);
                tracing::trace!(
                    err = %error_detail,
                    "invalid proto"
                );
                return vec![self.xds_ack(version, nonce, resource_type, Some(error_detail))];
            }
        };

        // handle the insert, on any error issue a NACK.
        let (changed_types, errors) = self
            .cache
            .insert(ResourceVersion::from(&version), resources);
        if tracing::enabled!(tracing::Level::TRACE) {
            let changed_types: Vec<_> = changed_types.values().collect();
            tracing::trace!(?changed_types, ?errors, "cache updated");
        }
        let mut responses = Vec::with_capacity(changed_types.len() + 1);

        // if everything is fine, update connection state to the newest version
        // and generate an ACK.
        //
        // if there are errors, don't update anything and generate a NACK.
        if errors.is_empty() {
            self.set_version_and_nonce(resource_type, version.clone(), nonce.clone());
            responses.push(self.xds_ack(version.clone(), nonce.clone(), resource_type, None));
        } else {
            // safety: we just checked on errors.is_empty()
            //
            // FIXME: format errors better for the connection, and maybe also log/return them?
            let error = errors.first().unwrap();
            responses.push(self.xds_ack(version, nonce, resource_type, Some(error.to_string())));
        }

        // also send out updated subscriptions for other types that have
        // changed. the ACK/NACK for the current type already contains the
        // subscription update.
        for changed_type in changed_types.values() {
            if changed_type == resource_type {
                continue;
            }
            responses.push(self.xds_subscription(changed_type))
        }

        responses
    }
}

fn xds_unknown_type(discovery_response: DiscoveryResponse) -> DiscoveryRequest {
    let message = format!("unknown type url: '{}'", discovery_response.type_url);
    let version_info = discovery_response.version_info;
    let response_nonce = discovery_response.nonce;
    let type_url = discovery_response.type_url;

    DiscoveryRequest {
        version_info,
        response_nonce,
        type_url,
        error_detail: Some(GrpcStatus {
            message,
            code: tonic::Code::InvalidArgument.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

impl<'a> AdsConnection<'a> {
    fn xds_ack(
        &self,
        version: String,
        nonce: String,
        resource_type: ResourceType,
        error_detail: Option<String>,
    ) -> DiscoveryRequest {
        let type_url = resource_type.type_url().to_string();
        let resource_names = self.cache.subscriptions(resource_type);
        let error_detail = error_detail.map(|message| GrpcStatus {
            message,
            code: tonic::Code::InvalidArgument.into(),
            ..Default::default()
        });

        DiscoveryRequest {
            version_info: version,
            response_nonce: nonce,
            type_url,
            resource_names,
            error_detail,
            ..Default::default()
        }
    }

    fn xds_subscription(&self, resource_type: ResourceType) -> DiscoveryRequest {
        let (version_info, response_nonce) = self.get_version_and_nonce(resource_type);
        let resource_names = self.cache.subscriptions(resource_type);

        DiscoveryRequest {
            type_url: resource_type.type_url().to_string(),
            resource_names,
            version_info,
            response_nonce,
            ..Default::default()
        }
    }

    #[inline]
    fn get_version_and_nonce(&self, type_id: ResourceType) -> (String, String) {
        let nonce = self.last_nonce[type_id].clone().unwrap_or_default();
        let version = self.resource_versions[type_id].clone().unwrap_or_default();
        (version, nonce)
    }

    #[inline]
    fn set_version_and_nonce(&mut self, type_id: ResourceType, version: String, nonce: String) {
        self.resource_versions[type_id] = Some(version);
        self.last_nonce[type_id] = Some(nonce);
    }
}

#[cfg(test)]
mod test_ads_conn {
    use super::test as xds_test;
    use super::*;

    #[test]
    fn test_initial_requests() {
        let mut cache = Cache::default();
        let (_, outgoing) = AdsConnection::new(&mut cache);
        assert!(outgoing.is_empty());

        let (tx, mut rx) = notify::new();
        cache.subscribe(
            ResourceType::Listener,
            "nginx.default.svc.cluster.local".to_string(),
            Instant::now(),
            tx,
        );
        let (_, outgoing) = AdsConnection::new(&mut cache);
        assert_eq!(
            outgoing,
            vec![xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["nginx.default.svc.cluster.local"]
            )]
        );
        assert!(rx.pending());

        cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "nginx.default.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["nginx.default.svc.cluster.local"],
                    [xds_test::route!(default "nginx.default.svc.cluster.local:80"),],
                )],
            )]),
        );
        assert!(rx.notified());

        let (_, outgoing) = AdsConnection::new(&mut cache);
        assert_eq!(
            outgoing,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    rs = vec!["nginx.default.svc.cluster.local:80"]
                ),
                xds_test::req!(
                    t = ResourceType::Listener,
                    rs = vec!["nginx.default.svc.cluster.local"]
                ),
            ]
        );

        cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "nginx.default.svc.cluster.local:80"),
            ]),
        );

        let (_, outgoing) = AdsConnection::new(&mut cache);
        assert_eq!(
            outgoing,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    rs = vec!["nginx.default.svc.cluster.local:80"]
                ),
                xds_test::req!(
                    t = ResourceType::ClusterLoadAssignment,
                    rs = vec!["nginx.default.svc.cluster.local:80"]
                ),
                xds_test::req!(
                    t = ResourceType::Listener,
                    rs = vec![
                        "nginx.default.svc.cluster.local",
                        "nginx.default.svc.cluster.local.lb.jct:80"
                    ]
                ),
            ]
        );
    }

    #[test]
    fn test_single_sub() {
        let mut cache = Cache::default();
        let (mut conn, _) = AdsConnection::new(&mut cache);

        let (add, mut rx) = SubscriptionUpdate::add(
            ResourceType::Listener,
            "nginx.default.svc.cluster.local".to_string(),
        );

        let request = conn.handle_subscription_update(Instant::now(), add);
        assert_eq!(
            request,
            Some(xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["nginx.default.svc.cluster.local"]
            )),
        );
        assert!(rx.pending());

        let (add, mut rx) =
            SubscriptionUpdate::add(ResourceType::Cluster, "default/nginx/cluster".to_string());
        let request = conn.handle_subscription_update(Instant::now(), add);
        assert_eq!(
            request,
            Some(xds_test::req!(
                t = ResourceType::Cluster,
                rs = vec!["default/nginx/cluster"]
            )),
        );
        assert!(rx.pending());
    }

    #[test]
    fn test_update_subs_on_incoming() {
        let mut cache = Cache::default();
        let (mut conn, _) = AdsConnection::new(&mut cache);

        let (add, mut rx) = SubscriptionUpdate::add(
            ResourceType::Listener,
            "nginx.default.svc.cluster.local".to_string(),
        );
        let request = conn.handle_subscription_update(Instant::now(), add);
        assert_eq!(
            request,
            Some(xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["nginx.default.svc.cluster.local"]
            ))
        );
        assert!(rx.pending());

        let requests = conn.handle_ads_message(xds_test::discovery_response(
            "v1",
            "n1",
            vec![xds_test::listener!(
                "nginx.default.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["nginx.default.svc.cluster.local"],
                    [xds_test::route!(default "nginx.default:80"),],
                )],
            )],
        ));

        assert!(rx.notified());
        assert_eq!(
            requests,
            vec![
                // ack the listener
                xds_test::discovery_request(
                    ResourceType::Listener,
                    "v1",
                    "n1",
                    vec!["nginx.default.svc.cluster.local"]
                ),
                // request the cluster that it targets. will have no version or nonce
                xds_test::discovery_request(
                    ResourceType::Cluster,
                    "",
                    "",
                    vec!["nginx.default:80"]
                ),
            ]
        );

        assert_eq!(
            conn.cache.subscriptions(ResourceType::Cluster),
            vec!["nginx.default:80"],
        );
    }

    #[test]
    fn test_ads_race() {
        let mut cache = Cache::default();
        let (mut conn, _) = AdsConnection::new(&mut cache);

        // subscribe to a listener, generate an XDS subscription for it
        let (add, mut rx) = SubscriptionUpdate::add(
            ResourceType::Listener,
            "nginx.default.svc.cluster.local".to_string(),
        );

        assert_eq!(
            conn.handle_subscription_update(Instant::now(), add),
            Some(xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["nginx.default.svc.cluster.local"]
            ))
        );
        assert!(rx.pending());

        // the LDS response includes two clusters
        let requests = conn.handle_ads_message(xds_test::discovery_response(
            "v1",
            "n1",
            vec![xds_test::listener!(
                "nginx.default.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["nginx.default.svc.cluster.local"],
                    [
                        xds_test::route!(header "x-staging" => "nginx-staging.default:80"),
                        xds_test::route!(default "nginx.default:80"),
                    ],
                )],
            )],
        ));

        assert!(rx.notified());
        assert_eq!(
            requests,
            vec![
                xds_test::req!(
                    t = ResourceType::Listener,
                    v = "v1",
                    n = "n1",
                    rs = vec!["nginx.default.svc.cluster.local"]
                ),
                xds_test::req!(
                    t = ResourceType::Cluster,
                    rs = vec!["nginx-staging.default:80", "nginx.default:80"]
                ),
            ]
        );

        // the first reply only includes a single cluster, the second one isn't
        // ready yet for some reason.
        //
        // should ACK with both the name of the current cluster and the one
        // we're still waiting for, and the first cluster should generate a sub
        // for the default listener.
        let requests = conn.handle_ads_message(xds_test::discovery_response(
            "v1",
            "n2",
            vec![xds_test::cluster!(eds "nginx.default:80")],
        ));
        assert_eq!(
            requests,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    v = "v1",
                    n = "n2",
                    rs = vec!["nginx-staging.default:80", "nginx.default:80"]
                ),
                xds_test::req!(
                    t = ResourceType::Listener,
                    v = "v1",
                    n = "n1",
                    rs = vec!["nginx.default.svc.cluster.local", "nginx.default.lb.jct:80"],
                ),
                xds_test::req!(
                    t = ResourceType::ClusterLoadAssignment,
                    rs = vec!["nginx.default:80"],
                ),
            ],
            "cluster request should include all resources. actual: {:#?}",
            requests,
        );

        // the second cluster appears!
        let requests = conn.handle_ads_message(xds_test::discovery_response(
            "v1",
            "n3",
            vec![
                xds_test::cluster!(eds "nginx.default:80"),
                xds_test::cluster!(eds "nginx-staging.default:80"),
            ],
        ));
        assert_eq!(
            requests,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    v = "v1",
                    n = "n3",
                    rs = vec!["nginx-staging.default:80", "nginx.default:80"]
                ),
                xds_test::req!(
                    t = ResourceType::Listener,
                    v = "v1",
                    n = "n1",
                    rs = vec![
                        "nginx.default.svc.cluster.local",
                        "nginx.default.lb.jct:80",
                        "nginx-staging.default.lb.jct:80",
                    ],
                ),
                xds_test::req!(
                    t = ResourceType::ClusterLoadAssignment,
                    rs = vec!["nginx.default:80", "nginx-staging.default:80"]
                ),
            ],
            "clusters should get acked again",
        );
    }
}
