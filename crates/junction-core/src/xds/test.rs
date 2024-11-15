//! Test macros for XDS Resources.
//!
//! Use these macros as a shorthand for writing out full XDS resource structs.

use std::str::FromStr;

use crate::xds::ResourceType;
use junction_api::{
    backend::{Backend, LbPolicy},
    BackendId,
};
use xds_api::pb::{
    envoy::{
        config::{
            cluster::v3 as xds_cluster,
            core::v3 as xds_core,
            endpoint::v3 as xds_endpoint,
            listener::v3 as xds_listener,
            route::v3::{self as xds_route, route_action::hash_policy::Header},
        },
        service::discovery::v3::{DiscoveryRequest, DiscoveryResponse},
    },
    google::protobuf,
};

use xds_api::pb::envoy::extensions::filters::{
    http::router::v3::Router, network::http_connection_manager::v3 as xds_http,
};

macro_rules! listener {
    ($name:expr, $route_name:expr$(,)*) => {{
        crate::xds::test::api_listener_rds($name, $route_name)
    }};
    ($name:expr => [$($vhost:expr),*$(,)*]$(,)?) => {{
        crate::xds::test::api_listener_inline_routes($name, vec![
            $(
                $vhost,
            )*
        ])
    }};
}

pub(crate) use listener;

macro_rules! vhost {
    ($name:expr, $domains:expr, [$($route:expr),*$(,)*]$(,)?) => {{
        crate::xds::test::virtual_host($name, $domains, vec![$($route,)*])
    }};
}

pub(crate) use vhost;

macro_rules! cluster {
    ($cluster_name:expr) => {{
        crate::xds::test::cluster_from_name($cluster_name, None)
    }};
    (ring_hash $cluster_name:expr) => {{
        crate::xds::test::cluster_from_name($cluster_name, Some(xds_cluster::cluster::LbPolicy::RingHash))
    }};
    (inline $cluster_name:expr => { $($region:expr => [$($addr:expr),*]),* }) => {{
        let cla = crate::xds::test::cluster_load_assignment($cluster_name, vec![$(
            crate::xds::test::locality_lb_endpoints(Some($region), None, vec![
                $(
                    crate::xds::test::lb_endpoint($addr, None, None),
                )+
            ]),
        )*]);

        crate::xds::test::cluster_inline($cluster_name, cla)
    }};
}

pub(crate) use cluster;

macro_rules! cla {
    ($cluster_name:expr => { $($region:expr => [$($addr:expr),*]),* }) => {{
        crate::xds::test::cluster_load_assignment($cluster_name, vec![$(
            crate::xds::test::locality_lb_endpoints(Some($region), None, vec![
                $(
                    crate::xds::test::lb_endpoint($addr, None, None),
                )+
            ]),
        )*])
    }};
}
pub(crate) use cla;

macro_rules! route_config {
    ($name:expr, $vhosts:expr) => {{
        xds_route::RouteConfiguration {
            name: $name.to_string(),
            virtual_hosts: $vhosts.into_iter().collect(),
            ..Default::default()
        }
    }};
}

pub(crate) use route_config;

macro_rules! route {
    (default $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path Some("/"), ring_hash None, header None => $cluster)
    }};
    (default ring_hash = $header:expr, $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path Some("/"), ring_hash Some($header), header None => $cluster)
    }};
    (header $header_name:expr => $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path None, ring_hash None, header Some($header_name) => $cluster)
    }};
    (path $path:expr => $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path Some($path), header None => $cluster)
    }};
    (_INTERNAL path $path:expr, ring_hash $hash_header:expr, header $header_name:expr => $cluster:expr) => {
        crate::xds::test::route_to_cluster($path, $hash_header, $header_name, $cluster)
    };
}

pub(crate) use route;

macro_rules! req {
    (t = $ty:expr, rs = $names:expr $(,)*) => {
        crate::xds::test::req!(t = $ty, v = "", n = "", rs = $names)
    };
    (t = $r:expr, v = $v:expr, n = $n:expr, rs = $names:expr $(,)*) => {{
        crate::xds::test::discovery_request($r, $v, $n, $names)
    }};
}

pub(crate) use req;

pub fn discovery_request(
    rtype: ResourceType,
    version_info: &'static str,
    response_nonce: &'static str,
    names: Vec<&'static str>,
) -> DiscoveryRequest {
    let names = names.into_iter().map(|n| n.to_string()).collect();
    DiscoveryRequest {
        type_url: rtype.type_url().to_string(),
        resource_names: names,
        version_info: version_info.to_string(),
        response_nonce: response_nonce.to_string(),
        ..Default::default()
    }
}

pub fn discovery_response<T: prost::Name>(
    version_info: &'static str,
    nonce: &'static str,
    resources: Vec<T>,
) -> DiscoveryResponse {
    let type_url = T::type_url();
    let resources = resources
        .into_iter()
        .map(|r| protobuf::Any::from_msg(&r).unwrap())
        .collect();
    DiscoveryResponse {
        type_url,
        version_info: version_info.to_string(),
        nonce: nonce.to_string(),
        resources,
        ..Default::default()
    }
}

pub fn api_listener_rds(name: &'static str, route_name: &'static str) -> xds_listener::Listener {
    use xds_http::{http_connection_manager::RouteSpecifier, http_filter::ConfigType, Rds};

    let http_router_filter = Router::default();
    let route_specifier = RouteSpecifier::Rds(Rds {
        config_source: Some(ads_config_source()),
        route_config_name: route_name.to_string(),
    });

    let http_connection_manager = xds_http::HttpConnectionManager {
        route_specifier: Some(route_specifier),
        http_filters: vec![xds_http::HttpFilter {
            name: "jct_connection_manager".to_string(),
            config_type: Some(ConfigType::TypedConfig(
                protobuf::Any::from_msg(&http_router_filter).expect("generated invalid xds"),
            )),
            ..Default::default()
        }],
        ..Default::default()
    };

    xds_listener::Listener {
        name: name.to_string(),
        api_listener: Some(xds_listener::ApiListener {
            api_listener: Some(protobuf::Any::from_msg(&http_connection_manager).unwrap()),
        }),
        ..Default::default()
    }
}

pub fn api_listener_inline_routes(
    name: &'static str,
    virtual_hosts: Vec<xds_route::VirtualHost>,
) -> xds_listener::Listener {
    use xds_http::{http_connection_manager::RouteSpecifier, http_filter::ConfigType};

    let http_router_filter = Router::default();
    let route_specifier = RouteSpecifier::RouteConfig(xds_route::RouteConfiguration {
        name: name.to_string(),
        virtual_hosts,
        ..Default::default()
    });

    let http_connection_manager = xds_http::HttpConnectionManager {
        route_specifier: Some(route_specifier),
        http_filters: vec![xds_http::HttpFilter {
            name: "jct_connection_manager".to_string(),
            config_type: Some(ConfigType::TypedConfig(
                protobuf::Any::from_msg(&http_router_filter).expect("generated invalid xds"),
            )),
            ..Default::default()
        }],
        ..Default::default()
    };

    xds_listener::Listener {
        name: name.to_string(),
        api_listener: Some(xds_listener::ApiListener {
            api_listener: Some(protobuf::Any::from_msg(&http_connection_manager).unwrap()),
        }),
        ..Default::default()
    }
}

pub fn route_to_cluster(
    path: Option<&str>,
    hash_header: Option<&str>,
    match_header: Option<&str>,
    cluster_name: &str,
) -> xds_route::Route {
    let mut route_match = xds_route::RouteMatch {
        ..Default::default()
    };

    if let Some(path) = path {
        route_match.path_specifier = Some(xds_route::route_match::PathSpecifier::Path(
            path.to_string(),
        ));
    }

    if let Some(header_name) = match_header {
        let header_matcher = xds_route::HeaderMatcher {
            name: header_name.to_string(),
            header_match_specifier: Some(
                xds_route::header_matcher::HeaderMatchSpecifier::PresentMatch(true),
            ),
            ..Default::default()
        };
        route_match.headers = vec![header_matcher];
    }

    let hash_policy = hash_header
        .map(|header_name| {
            let hash_policy = xds_route::route_action::HashPolicy {
                policy_specifier: Some(
                    xds_route::route_action::hash_policy::PolicySpecifier::Header(Header {
                        header_name: header_name.to_string(),
                        regex_rewrite: None,
                    }),
                ),
                terminal: true,
            };
            vec![hash_policy]
        })
        .unwrap_or_default();

    let action = xds_route::route::Action::Route(xds_route::RouteAction {
        hash_policy,
        cluster_specifier: Some(xds_route::route_action::ClusterSpecifier::Cluster(
            cluster_name.to_string(),
        )),
        ..Default::default()
    });
    xds_route::Route {
        r#match: Some(route_match),
        action: Some(action),
        ..Default::default()
    }
}

pub fn virtual_host(
    name: &'static str,
    domains: impl IntoIterator<Item = &'static str>,
    routes: impl IntoIterator<Item = xds_route::Route>,
) -> xds_route::VirtualHost {
    xds_route::VirtualHost {
        name: name.to_string(),
        domains: domains.into_iter().map(|s| s.to_string()).collect(),
        routes: routes.into_iter().collect(),
        ..Default::default()
    }
}

pub fn cluster_from_name(
    name: &'static str,
    lb_policy: Option<xds_cluster::cluster::LbPolicy>,
) -> xds_cluster::Cluster {
    let backend = Backend {
        id: BackendId::from_str(name).unwrap(),
        lb: LbPolicy::Unspecified,
    };

    let mut cluster = backend.to_xds_cluster();
    if let Some(lb_policy) = lb_policy {
        cluster.lb_policy = lb_policy.into();
    }
    cluster
}

pub fn cluster_inline(
    name: &'static str,
    cla: xds_endpoint::ClusterLoadAssignment,
) -> xds_cluster::Cluster {
    xds_cluster::Cluster {
        name: name.to_string(),
        cluster_discovery_type: None,
        load_assignment: Some(cla),
        ..Default::default()
    }
}

pub fn cluster_load_assignment(
    name: &'static str,
    endpoints: Vec<xds_endpoint::LocalityLbEndpoints>,
) -> xds_endpoint::ClusterLoadAssignment {
    xds_endpoint::ClusterLoadAssignment {
        cluster_name: name.to_string(),
        endpoints,
        ..Default::default()
    }
}

pub fn locality_lb_endpoints(
    region: Option<&'static str>,
    zone: Option<&'static str>,
    lb_endpoints: Vec<xds_endpoint::LbEndpoint>,
) -> xds_endpoint::LocalityLbEndpoints {
    let locality = xds_core::Locality {
        region: region.unwrap_or("").to_string(),
        zone: zone.unwrap_or("").to_string(),
        ..Default::default()
    };

    xds_endpoint::LocalityLbEndpoints {
        locality: Some(locality),
        lb_endpoints,
        ..Default::default()
    }
}

pub fn lb_endpoint(
    hostname: &'static str,
    port: Option<u32>,
    health: Option<xds_core::HealthStatus>,
) -> xds_endpoint::LbEndpoint {
    let port = port.unwrap_or(80);
    let endpoint = xds_endpoint::Endpoint {
        address: Some(xds_core::Address {
            address: Some(xds_core::address::Address::SocketAddress(
                xds_core::SocketAddress {
                    address: hostname.to_string(),
                    port_specifier: Some(xds_core::socket_address::PortSpecifier::PortValue(port)),
                    ..Default::default()
                },
            )),
        }),
        ..Default::default()
    };

    let health = health.unwrap_or(xds_core::HealthStatus::Healthy);
    xds_endpoint::LbEndpoint {
        health_status: health as i32,
        metadata: None,
        load_balancing_weight: None,
        host_identifier: Some(xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(
            endpoint,
        )),
    }
}

pub fn ads_config_source() -> xds_core::ConfigSource {
    xds_core::ConfigSource {
        config_source_specifier: Some(xds_core::config_source::ConfigSourceSpecifier::Ads(
            xds_core::AggregatedConfigSource {},
        )),
        resource_api_version: xds_core::ApiVersion::V3 as i32,
        ..Default::default()
    }
}
