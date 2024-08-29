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
use xds_api::pb::envoy::service::status::v3::{
    client_config::GenericXdsConfig,
    client_status_discovery_service_server::{
        ClientStatusDiscoveryService, ClientStatusDiscoveryServiceServer,
    },
    ClientConfig, ClientStatusRequest, ClientStatusResponse,
};

use crate::xds::CacheReader;

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
        let generic_xds_configs: Vec<_> = self
            .cache
            .iter_any()
            .map(|(name, any_pb)| {
                let type_url = any_pb.type_url.clone();
                GenericXdsConfig {
                    name,
                    type_url,
                    xds_config: Some(any_pb),
                    ..Default::default()
                }
            })
            .collect();

        Ok(Response::new(ClientStatusResponse {
            config: vec![ClientConfig {
                node,
                generic_xds_configs,
                ..Default::default()
            }],
        }))
    }
}
