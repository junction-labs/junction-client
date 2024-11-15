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
use cache::{Cache, CacheReader};
use enum_map::EnumMap;
use futures::TryStreamExt;
use junction_api::{http::Route, BackendId, Hostname, Target, VirtualHost};
use std::{collections::BTreeSet, future::Future, io::ErrorKind, sync::Arc, time::Duration};
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
    google::protobuf,
    google::rpc::Status as GrpcStatus,
};

mod cache;

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
    pub version: ResourceVersion,
    pub xds: Option<protobuf::Any>,
    pub last_error: Option<(ResourceVersion, String)>,
}

#[derive(Debug)]
enum SubscriptionUpdate {
    AddVirtualHosts(Vec<VirtualHost>),
    RemoveVirtualHosts(Vec<VirtualHost>),
    AddBackends(Vec<BackendId>),
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
        let cache = Cache::default();

        // FIXME: make this configurable
        let dns = StdlibResolver::new_with(Duration::from_secs(5), Duration::from_millis(500), 2);

        let client = AdsClient {
            subs: sub_tx,
            cache: cache.reader(),
            dns: dns.clone(),
        };
        let task = AdsTask {
            endpoint,
            initial_channel: None,
            node_info,
            cache,
            dns,
            subs: sub_rx,
        };

        Ok((client, task))
    }

    pub(super) fn subscribe_to_vhosts(
        &self,
        vhosts: impl IntoIterator<Item = VirtualHost>,
    ) -> Result<(), ()> {
        let vhosts = vhosts.into_iter().collect();
        self.subs
            .try_send(SubscriptionUpdate::AddVirtualHosts(vhosts))
            .map_err(|_| ())?;

        Ok(())
    }

    // TODO: call this on client dropping defaults
    #[allow(unused)]
    pub(super) fn unsubscribe_from_vhosts(
        &self,
        vhosts: impl IntoIterator<Item = VirtualHost>,
    ) -> Result<(), ()> {
        let vhosts = vhosts.into_iter().collect();
        self.subs
            .try_send(SubscriptionUpdate::RemoveVirtualHosts(vhosts))
            .map_err(|_| ())?;

        Ok(())
    }

    pub(super) fn subscribe_to_backends(
        &self,
        backends: impl IntoIterator<Item = BackendId>,
    ) -> Result<(), ()> {
        let backends = backends.into_iter().collect();
        self.subs
            .try_send(SubscriptionUpdate::AddBackends(backends))
            .map_err(|_| ())?;

        Ok(())
    }

    // TODO: call this on client dropping defaults
    #[allow(unused)]
    pub(super) fn unsubscribe_from_backends(
        &self,
        backends: impl IntoIterator<Item = BackendId>,
    ) -> Result<(), ()> {
        let backends = backends.into_iter().collect();
        self.subs
            .try_send(SubscriptionUpdate::RemoveBackends(backends))
            .map_err(|_| ())?;

        Ok(())
    }

    pub(super) fn csds_server(
        &self,
        port: u16,
    ) -> impl Future<Output = Result<(), tonic::transport::Error>> {
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
    fn get_route(
        &self,
        target: &junction_api::VirtualHost,
    ) -> Option<std::sync::Arc<junction_api::http::Route>> {
        self.cache.get_route(target)
    }

    fn get_backend(
        &self,
        target: &junction_api::BackendId,
    ) -> Option<std::sync::Arc<crate::BackendLb>> {
        self.cache.get_backend(target)
    }

    fn get_endpoints(
        &self,
        backend: &junction_api::BackendId,
    ) -> Option<std::sync::Arc<crate::load_balancer::EndpointGroup>> {
        match &backend.target {
            junction_api::Target::Dns(dns) => self.dns.get_endpoints(&dns.hostname, backend.port),
            _ => self.cache.get_endpoints(backend),
        }
    }
}

pub(crate) struct AdsTask {
    endpoint: tonic::transport::Endpoint,
    initial_channel: Option<tonic::transport::Channel>,
    node_info: xds_core::Node,
    cache: Cache,
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

macro_rules! trace_xds_request {
    ($request:expr) => {
        tracing::debug!(
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
        tracing::debug!(
            "DiscoveryResponse(v={:?}, n={:?}, ty={:?}, r_count={:?})",
            $response.version_info,
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
        let channel = self.new_connection().await?;
        let mut client = AggregatedDiscoveryServiceClient::new(channel);

        let (xds_tx, xds_rx) = tokio::sync::mpsc::channel(10);
        let stream_response = client
            .stream_aggregated_resources(ReceiverStream::new(xds_rx))
            .await?;

        let mut incoming = stream_response.into_inner();
        let (mut conn, outgoing) = AdsConnection::new(self.node_info.clone(), &mut self.cache);

        // always sync dns from cache at the start. there will be no names to
        // add/remove individually since there were no incoming subscription
        // changes.
        update_dns(
            &self.dns,
            BTreeSet::new(),
            BTreeSet::new(),
            Some(conn.cache.dns_names()),
        );

        // send messages
        send_outgoing(&mut conn, &xds_tx, outgoing).await;

        // TODO: figure out how to do some batching here. we think it will
        // reduce xDS chatter
        loop {
            let outgoing = tokio::select! {
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
                    tracing::trace!("handling ads message");
                    conn.handle_ads_message(response)
                }
                sub_update = self.subs.recv() => {
                    let Some(sub_update) = sub_update else {
                        return Ok(())
                    };
                    tracing::trace!(
                        ?sub_update,
                        "handling subscrition update",
                    );
                    conn.handle_subscription_update(sub_update)
                }
            };

            // update dns
            let updates = conn.dns_updates();
            // sync.then(...) takes a closure and won't build the iterator
            // unless there are changes.
            update_dns(
                &self.dns,
                updates.add,
                updates.remove,
                updates.sync.then(|| conn.cache.dns_names()),
            );

            // send messages
            send_outgoing(&mut conn, &xds_tx, outgoing).await;
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
async fn send_outgoing(
    conn: &mut AdsConnection<'_>,
    tx: &tokio::sync::mpsc::Sender<DiscoveryRequest>,
    outgoing: Vec<DiscoveryRequest>,
) {
    for mut msg in outgoing {
        if let Some(node) = conn.node.take() {
            msg.node = Some(node)
        }
        trace_xds_request!(msg);
        tx.send(msg).await.unwrap();
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

/// The state of an individual ADS connection. This is mostly the cache, but has
/// to do some bookkeeping around nonces and resource versions for every
/// resource type.
///
/// This struct also uses itself as a state accumulator for the side effects of
/// handling messages and subscriptions.
///
/// Sending xDS messages isn't done by accumulating an outbox since we haven't
/// sat down and thought through what it means to coalesce xDS responses. It
/// should be though, since batching will be huge for reducing chattiness over
/// the wire.
///
// TODO: start accumulating xds outbox here once we figure out how to
// coalesce multiple updates into a single batch of requests.
#[derive(Debug)]
struct AdsConnection<'a> {
    cache: &'a mut Cache,
    last_nonce: EnumMap<ResourceType, Option<String>>,
    resource_versions: EnumMap<ResourceType, Option<String>>,

    // the xDS node info associated with this connection. should be removed and
    // sent with the first message on this connection.
    node: Option<xds_core::Node>,

    // the list of DNS names to subscribe to
    dns_subscribes: BTreeSet<(Hostname, u16)>,

    // the list of DNS names to unsubscribe from.
    dns_unsubscribes: BTreeSet<(Hostname, u16)>,

    // if true, synchronzie the set of DNS names from the cache to the DNS resolver.
    dns_sync_from_cache: bool,
}

#[derive(Debug)]
struct DnsUpdates {
    add: BTreeSet<(Hostname, u16)>,
    remove: BTreeSet<(Hostname, u16)>,
    sync: bool,
}

impl<'a> AdsConnection<'a> {
    fn new(node: xds_core::Node, cache: &'a mut Cache) -> (Self, Vec<DiscoveryRequest>) {
        let conn = Self {
            last_nonce: EnumMap::default(),
            resource_versions: EnumMap::default(),
            cache,
            node: Some(node),
            dns_subscribes: BTreeSet::new(),
            dns_unsubscribes: BTreeSet::new(),
            dns_sync_from_cache: false,
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
    // inputs

    fn handle_subscription_update(&mut self, reg: SubscriptionUpdate) -> Vec<DiscoveryRequest> {
        fn target_dns_name(target: &Target) -> Option<Hostname> {
            match target {
                Target::Dns(dns) => Some(dns.hostname.clone()),
                _ => None,
            }
        }

        let rtype;
        let mut changed = false;
        match reg {
            SubscriptionUpdate::AddVirtualHosts(vhosts) => {
                rtype = ResourceType::Listener;
                for vhost in vhosts {
                    changed |= self.cache.subscribe(rtype, &vhost.name());
                }
            }
            SubscriptionUpdate::RemoveVirtualHosts(vhosts) => {
                rtype = ResourceType::Listener;
                for vhost in vhosts {
                    changed |= self.cache.delete(rtype, &vhost.name());
                }
            }
            SubscriptionUpdate::AddBackends(backends) => {
                rtype = ResourceType::Cluster;
                for backend in backends {
                    if let Some(dns_name) = target_dns_name(&backend.target) {
                        self.dns_subscribes.insert((dns_name, backend.port));
                    }
                    changed |= self.cache.subscribe(rtype, &backend.name())
                }
            }
            SubscriptionUpdate::RemoveBackends(backends) => {
                rtype = ResourceType::Listener;
                for backend in backends {
                    if let Some(dns_name) = target_dns_name(&backend.target) {
                        self.dns_unsubscribes.insert((dns_name, backend.port));
                    }
                    changed |= self.cache.delete(rtype, &backend.name())
                }
            }
        }

        if changed {
            vec![self.xds_subscription(rtype)]
        } else {
            Vec::new()
        }
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

        // now that we have a type, immediately set the last recognized nonce so
        // that any future messages dont' get dropped even if this is a NACK
        let (version, nonce) = (discovery_response.version_info, discovery_response.nonce);
        self.set_nonce(resource_type, nonce.clone());

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

        // handle the insert
        let (changed_types, errors) = self
            .cache
            .insert(ResourceVersion::from(&version), resources);

        if tracing::enabled!(tracing::Level::TRACE) {
            let changed_types: Vec<_> = changed_types.values().collect();
            tracing::trace!(?changed_types, ?errors, "cache updated");
        }

        // start building responses
        let mut responses = Vec::with_capacity(changed_types.len() + 1);

        // if everything is fine, update connection state to the newest version
        // and generate an ACK.
        //
        // if there are errors, don't update anything and generate a NACK.
        if errors.is_empty() {
            self.set_version(resource_type, version.clone());
            responses.push(self.xds_ack(version.clone(), nonce, resource_type, None));
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

        // if there was a cluster change, it's possible that it's time to sync DNS
        if changed_types.contains(ResourceType::Cluster) {
            self.dns_sync_from_cache = true;
        }

        responses
    }

    // outputs

    fn dns_updates(&mut self) -> DnsUpdates {
        let add = std::mem::take(&mut self.dns_subscribes);
        let remove = std::mem::take(&mut self.dns_unsubscribes);
        let sync = std::mem::take(&mut self.dns_sync_from_cache);
        DnsUpdates { add, remove, sync }
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
    fn set_version(&mut self, type_id: ResourceType, version: String) {
        self.resource_versions[type_id] = Some(version);
    }

    #[inline]
    fn set_nonce(&mut self, type_id: ResourceType, nonce: String) {
        self.last_nonce[type_id] = Some(nonce);
    }
}

#[cfg(test)]
mod test_ads_conn {
    use once_cell::sync::Lazy;

    use super::test as xds_test;
    use super::*;

    static TEST_NODE: Lazy<xds_core::Node> = Lazy::new(|| xds_core::Node {
        id: "unit-test".to_string(),
        ..Default::default()
    });

    #[inline]
    fn new_conn(cache: &mut Cache) -> (AdsConnection, Vec<DiscoveryRequest>) {
        AdsConnection::new(TEST_NODE.clone(), cache)
    }

    #[track_caller]
    fn assert_dns_sync(conn: &mut AdsConnection<'_>) {
        let updates = conn.dns_updates();
        assert!(
            updates.add.is_empty(),
            "added subscriptions should be empty"
        );
        assert!(
            updates.remove.is_empty(),
            "removed subscriptions should be empty"
        );
        assert!(updates.sync, "should be syncing dns");
    }

    #[test]
    fn test_initial_requests() {
        let mut cache = Cache::default();
        let (_, outgoing) = new_conn(&mut cache);
        assert!(outgoing.is_empty());

        cache.subscribe(ResourceType::Listener, "nginx.default.svc.cluster.local");
        let (_, outgoing) = new_conn(&mut cache);
        assert_eq!(
            outgoing,
            vec![xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["nginx.default.svc.cluster.local"]
            )]
        );

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

        let (_, outgoing) = new_conn(&mut cache);
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
            ResourceVec::Cluster(vec![xds_test::cluster!(
                "nginx.default.svc.cluster.local:80"
            )]),
        );

        let (_, outgoing) = new_conn(&mut cache);
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
    fn test_subscribe() {
        let mut cache = Cache::default();
        let (mut conn, _) = new_conn(&mut cache);

        let request = conn.handle_subscription_update(SubscriptionUpdate::AddVirtualHosts(vec![
            Target::dns("website.internal").unwrap().into_vhost(None),
            Target::kube_service("default", "nginx")
                .unwrap()
                .into_vhost(None),
        ]));

        // dns shouldn't update for routes
        let updates = conn.dns_updates();
        assert!(
            updates.add.is_empty() && updates.remove.is_empty() && !updates.sync,
            "dns should not update"
        );
        assert_eq!(
            request,
            vec![xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["website.internal", "nginx.default.svc.cluster.local",]
            ),],
        );

        let request = conn.handle_subscription_update(SubscriptionUpdate::AddBackends(vec![
            Target::dns("website.internal").unwrap().into_backend(4567),
            Target::kube_service("default", "nginx")
                .unwrap()
                .into_backend(8080),
        ]));
        assert_eq!(
            request,
            vec![xds_test::req!(
                t = ResourceType::Cluster,
                rs = vec![
                    "website.internal:4567",
                    "nginx.default.svc.cluster.local:8080",
                ]
            )],
        );

        // dns should update only for the dns vhost
        let updates = conn.dns_updates();
        assert_eq!(
            updates.add,
            BTreeSet::from_iter([(Hostname::from_static("website.internal"), 4567)]),
        );
        assert!(
            updates.remove.is_empty() && !updates.sync,
            "should be no DNS removes or sync"
        );
    }

    #[test]
    fn test_update_subs_on_incoming() {
        let mut cache = Cache::default();
        let (mut conn, _) = new_conn(&mut cache);

        let requests = conn.handle_subscription_update(SubscriptionUpdate::AddVirtualHosts(vec![
            Target::kube_service("default", "nginx")
                .unwrap()
                .into_vhost(None),
        ]));
        let updates = conn.dns_updates();
        assert!(
            updates.add.is_empty() && updates.remove.is_empty() && !updates.sync,
            "should be no DNS changes",
        );
        assert_eq!(
            requests,
            vec![xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["nginx.default.svc.cluster.local"]
            )],
        );

        let requests = conn.handle_ads_message(xds_test::discovery_response(
            "v1",
            "n1",
            vec![xds_test::listener!(
                "nginx.default.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["nginx.default.svc.cluster.local"],
                    [xds_test::route!(default "nginx.internal:80"),],
                )],
            )],
        ));

        // dns changes every time a cluster changes
        assert_dns_sync(&mut conn);
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
                    vec!["nginx.internal:80"]
                ),
            ]
        );

        assert_eq!(
            conn.cache.subscriptions(ResourceType::Cluster),
            vec!["nginx.internal:80"],
        );
    }

    #[test]
    fn test_ads_race() {
        let mut cache = Cache::default();
        let (mut conn, _) = new_conn(&mut cache);

        // subscribe to a listener, generate an XDS subscription for it
        let requests = conn.handle_subscription_update(SubscriptionUpdate::AddVirtualHosts(vec![
            Target::kube_service("default", "nginx")
                .unwrap()
                .into_vhost(None),
        ]));
        assert_eq!(
            requests,
            vec![xds_test::req!(
                t = ResourceType::Listener,
                rs = vec!["nginx.default.svc.cluster.local"]
            )],
        );

        // the LDS response includes two clusters
        let requests = conn.handle_ads_message(xds_test::discovery_response(
            "v1",
            "n1",
            vec![xds_test::listener!(
                "nginx.default.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["nginx.default.svc.cluster.local"],
                    [
                        xds_test::route!(header "x-staging" => "nginx-staging.internal:80"),
                        xds_test::route!(default "nginx.internal:80"),
                    ],
                )],
            )],
        ));

        // dns could change every time cluster names change, which changes
        // because of LDS route pointers.
        assert_dns_sync(&mut conn);
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
                    rs = vec!["nginx-staging.internal:80", "nginx.internal:80"]
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
            vec![xds_test::cluster!("nginx.internal:80")],
        ));

        assert_dns_sync(&mut conn);
        assert_eq!(
            requests,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    v = "v1",
                    n = "n2",
                    rs = vec!["nginx-staging.internal:80", "nginx.internal:80"]
                ),
                xds_test::req!(
                    t = ResourceType::Listener,
                    v = "v1",
                    n = "n1",
                    rs = vec![
                        "nginx.default.svc.cluster.local",
                        "nginx.internal.lb.jct:80"
                    ],
                ),
                xds_test::req!(
                    t = ResourceType::ClusterLoadAssignment,
                    rs = vec!["nginx.internal:80"],
                ),
            ],
            "cluster request should include all resources. actual: {:#?}",
            requests,
        );

        // the second cluster appears! also a DNS name, so we have a DNS update again
        let requests = conn.handle_ads_message(xds_test::discovery_response(
            "v1",
            "n3",
            vec![
                xds_test::cluster!("nginx.internal:80"),
                xds_test::cluster!("nginx-staging.internal:80"),
            ],
        ));

        assert_dns_sync(&mut conn);
        assert_eq!(
            requests,
            vec![
                xds_test::req!(
                    t = ResourceType::Cluster,
                    v = "v1",
                    n = "n3",
                    rs = vec!["nginx-staging.internal:80", "nginx.internal:80"]
                ),
                xds_test::req!(
                    t = ResourceType::Listener,
                    v = "v1",
                    n = "n1",
                    rs = vec![
                        "nginx.default.svc.cluster.local",
                        "nginx.internal.lb.jct:80",
                        "nginx-staging.internal.lb.jct:80",
                    ],
                ),
                xds_test::req!(
                    t = ResourceType::ClusterLoadAssignment,
                    rs = vec!["nginx.internal:80", "nginx-staging.internal:80"]
                ),
            ],
            "clusters should get acked again",
        );
    }
}
