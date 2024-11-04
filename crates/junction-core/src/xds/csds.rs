//! A server for [CSDS][csds].
//!
//! [csds]: https://www.envoyproxy.io/docs/envoy/latest/api-v3/service/status/v3/csds.proto

use std::{
    net::SocketAddr,
    net::{IpAddr, Ipv4Addr},
    pin::Pin,
};

use futures::Stream;
use tonic::{Request, Response, Status, Streaming};
use xds_api::pb::envoy::{
    admin::v3::ClientResourceStatus,
    service::status::v3::{
        client_config::GenericXdsConfig,
        client_status_discovery_service_server::{
            ClientStatusDiscoveryService, ClientStatusDiscoveryServiceServer,
        },
        ClientConfig, ClientStatusRequest, ClientStatusResponse,
    },
};

use crate::xds::{CacheReader, XdsConfig};

/// Run a CSDS server listening on `localhost` at the given port.
pub async fn local_server(cache: CacheReader, port: u16) -> Result<(), tonic::transport::Error> {
    let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(xds_api::FILE_DESCRIPTOR_SET)
        .with_service_name("envoy.service.status.v3.ClientStatusDiscoveryService")
        .build()
        .unwrap();

    tonic::transport::Server::builder()
        .add_service(reflection)
        .add_service(ClientStatusDiscoveryServiceServer::new(Server { cache }))
        .serve(socket_addr)
        .await
}

/// A CSDS Server that exposes the current state of a client's cache.
///
/// Unlike a standard CSDS server, this server only has a single node worth of
/// config to expose. Because there is no reasonable way to interpret a node
/// matcher, any request that sets `node_matchers` will return an error.
struct Server {
    cache: CacheReader,
}

type ClientStatusResponseStream =
    Pin<Box<dyn Stream<Item = Result<ClientStatusResponse, Status>> + Send>>;

#[tonic::async_trait]
impl ClientStatusDiscoveryService for Server {
    type StreamClientStatusStream = ClientStatusResponseStream;

    async fn stream_client_status(
        &self,
        _request: Request<Streaming<ClientStatusRequest>>,
    ) -> Result<Response<Self::StreamClientStatusStream>, Status> {
        return Err(Status::unimplemented(
            "streaming client status is not supported",
        ));
    }

    async fn fetch_client_status(
        &self,
        request: Request<ClientStatusRequest>,
    ) -> Result<Response<ClientStatusResponse>, Status> {
        let request = request.into_inner();

        if !request.node_matchers.is_empty() {
            return Err(Status::invalid_argument(
                "node_matchers are unsupported for a single client CSDS endpoint",
            ));
        }

        let node = request.node;
        let generic_xds_configs: Vec<_> = self.cache.iter_xds().map(to_generic_config).collect();

        Ok(Response::new(ClientStatusResponse {
            config: vec![ClientConfig {
                node,
                generic_xds_configs,
                ..Default::default()
            }],
        }))
    }
}

/// Convert a crate config to an xDS generic config
///
/// There's no way on GenericXdsConfig to indicate that you've ACKed one version
/// of a config but rejected another. Since xDS generally gets cranky when you
/// specify a duplicate resource name, we're currently just only showing an error
/// if there was any error at all.
///
/// This is weird but so is xDS. There's a hidden field that describes the last
/// error trying to apply this config that would do what we want, but it's hidden
/// as not-implemented, so not in our protobufs.
///
/// Either figure out how to use the hidden field, or return MULTIPLE statuses for
/// resources that have a valid and an invalid resource.
fn to_generic_config(config: XdsConfig) -> GenericXdsConfig {
    let client_status = match (&config.xds, &config.last_error) {
        (_, Some(_)) => ClientResourceStatus::Nacked,
        (Some(_), None) => ClientResourceStatus::Acked,
        _ => ClientResourceStatus::Unknown,
    };

    GenericXdsConfig {
        type_url: config.type_url,
        name: config.name,
        version_info: config.version.to_string(),
        xds_config: config.xds,
        client_status: client_status.into(),
        ..Default::default()
    }
}
