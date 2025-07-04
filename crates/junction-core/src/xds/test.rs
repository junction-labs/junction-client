//! Test macros for XDS Resources.
//!
//! Use these macros as a shorthand for writing out full XDS resource structs.

use crate::xds::{resources::ads_config_source, ResourceType};
use xds_api::pb::{
    envoy::{
        config::{
            cluster::v3 as xds_cluster,
            core::v3 as xds_core,
            endpoint::v3 as xds_endpoint,
            listener::v3 as xds_listener,
            route::v3::{self as xds_route, route_action::hash_policy::Header},
        },
        r#type::matcher::v3 as xds_matcher,
        service::discovery::v3::{
            self as xds_discovery, DeltaDiscoveryRequest, DeltaDiscoveryResponse,
        },
    },
    google::{protobuf, rpc},
};

use xds_api::pb::envoy::extensions::filters::{
    http::router::v3::Router, network::http_connection_manager::v3 as xds_http,
};

macro_rules! listener {
    ($name:expr, $route_name:expr$(,)*) => {
        crate::xds::test::listener!($name, "v123", $route_name)
    };
    ($name:expr, $version:expr, $route_name:expr$(,)*) => {{
        let listener = crate::xds::test::api_listener_rds($name, $route_name);
        crate::xds::test::xds_resource($name.to_string(), $version.to_string(), listener)
    }};
    ($name:expr, $route_name:expr => [$($vhost:expr),*$(,)*]$(,)?) =>  {
        crate::xds::test::listener!($name, "v123", $route_name => [$($vhost,)*])
    };
    ($name:expr, $version:expr, $route_name:expr => [$($vhost:expr),*$(,)*]$(,)?) => {{
        let listener = crate::xds::test::api_listener_inline_routes($name, $route_name, vec![
            $(
                $vhost,
            )*
        ]);
        crate::xds::test::xds_resource($name.to_string(), $version.to_string(), listener)
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
    (eds => $cluster_name:expr) => {
        crate::xds::test::cluster!(eds => $cluster_name, "v123")
    };
    (eds => $cluster_name:expr, $version:expr) => {{
        let cluster = crate::xds::test::eds_cluster($cluster_name, None);
        crate::xds::test::xds_resource($cluster_name.to_string(), $version.to_string(), cluster)
    }};
    (logical_dns => $hostname:expr, $port:expr) => {
        crate::xds::test::cluster!(logical_dns => $hostname, $port, "v123")
    };
    (logical_dns => $hostname:expr, $port:expr, $version:expr) => {{
        let cluster_name = format!("{}:{}", $hostname, $port);
        let cluster = crate::xds::test::logical_dns_cluster(&cluster_name, $hostname, $port, None);
        crate::xds::test::xds_resource(cluster_name.to_string(), $version.to_string(), cluster)
    }};
}

pub(crate) use cluster;

macro_rules! cla {
    ($name:expr => { $($region:expr => [$($addr:expr),*]),* }) => {
        crate::xds::test::cla!($name, "v123" => {
            $(
                $region => [$($addr),*]
            ),*
        })
    };
    ($name:expr, $version:expr => { $($region:expr => [$($addr:expr),*]),* }) => {{
        let cla = crate::xds::test::cluster_load_assignment($name, vec![$(
            crate::xds::test::locality_lb_endpoints(Some($region), None, vec![
                $(
                    crate::xds::test::lb_endpoint($addr, None, None),
                )+
            ]),
        )*]);
        crate::xds::test::xds_resource($name.to_string(), $version.to_string(), cla)
    }};
}
pub(crate) use cla;

macro_rules! route_config {
    ($name:expr, $vhosts:expr) => {
        crate::xds::test::route_config!($name, "v123", $vhosts)
    };
    ($name:expr, $version:expr, $vhosts:expr) => {{
        let rc = xds_api::pb::envoy::config::route::v3::RouteConfiguration {
            name: $name.to_string(),
            virtual_hosts: $vhosts.into_iter().collect(),
            ..Default::default()
        };
        crate::xds::test::xds_resource($name.to_string(), $version.to_string(), rc)
    }};
}

pub(crate) use route_config;

macro_rules! route {
    (default $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path None, ring_hash None, header None, query None => $cluster)
    }};
    (default ring_hash = $header:expr, $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path None, ring_hash Some($header), header None, query None => $cluster)
    }};
    (header $header_name:expr => $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path None, ring_hash None, header Some($header_name), query None => $cluster)
    }};
    (exact_path $path:expr => $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path Some($path), ring_hash None, header None, query None => $cluster)
    }};
    (query $name:expr, $value:expr => $cluster:expr) => {{
        crate::xds::test::route!(_INTERNAL path None, ring_hash None, header None, query Some(($name, $value))=> $cluster)
    }};
    (
        _INTERNAL
        path $path:expr,
        ring_hash $hash_header:expr,
        header $header_name:expr,
        query $query:expr
        => $cluster:expr
    ) => {
        crate::xds::test::route_to_cluster($path, $hash_header, $header_name, $query, $cluster)
    };
}

pub(crate) use route;

macro_rules! req {
    (t = $ty:expr $(,)*) => {
        crate::xds::test::req!(
            t = $ty,
            n = "",
            add = vec![],
            remove = vec![],
            init = vec![],
            err = None
        )
    };
    (t = $ty:expr, n = $n:expr $(,)*) => {
        crate::xds::test::req!(
            t = $ty,
            n = $n,
            add = vec![],
            remove = vec![],
            init = vec![],
            err = None
        )
    };
    (t = $ty:expr, add = $add:expr $(,)*) => {
        crate::xds::test::req!(
            t = $ty,
            n = "",
            add = $add,
            remove = vec![],
            init = vec![],
            err = None
        )
    };
    (t = $ty:expr, remove = $remove:expr $(,)*) => {
        crate::xds::test::req!(
            t = $ty,
            n = "",
            add = vec![],
            remove = $remove,
            init = vec![],
            err = None
        )
    };
    (t = $ty:expr, n = $n:expr, add = $add:expr $(,)*) => {
        crate::xds::test::req!(
            t = $ty,
            n = $n,
            add = $add,
            remove = vec![],
            init = vec![],
            err = None
        )
    };
    (t = $ty:expr, n = $n:expr, add = $add:expr, remove = $remove:expr $(,)*) => {
        crate::xds::test::req!(
            t = $ty,
            n = $n,
            add = $add,
            remove = $remove,
            init = vec![],
            err = None
        )
    };
    (t = $ty:expr, add = $add:expr, init = $init:expr $(,)*) => {
        crate::xds::test::req!(
            t = $ty,
            n = "",
            add = $add,
            remove = vec![],
            init = $init,
            err = None
        )
    };
    (t = $rty:expr, n = $n:expr, add = $add:expr, remove = $remove:expr, init = $init:expr, err = $err:expr $(,)*) => {{
        crate::xds::test::delta_discovery_request($rty, $n, $add, $remove, $init, $err)
    }};
}

pub(crate) use req;

pub fn delta_discovery_request(
    rtype: ResourceType,
    response_nonce: &'static str,
    subscribe: Vec<&'static str>,
    unsubscribe: Vec<&'static str>,
    versions: Vec<(&'static str, &'static str)>,
    error: Option<&'static str>,
) -> DeltaDiscoveryRequest {
    let resource_names_subscribe = subscribe.into_iter().map(|n| n.to_string()).collect();
    let resource_names_unsubscribe = unsubscribe.into_iter().map(|n| n.to_string()).collect();
    let initial_resource_versions = versions
        .into_iter()
        .map(|(v, r)| (v.to_string(), r.to_string()))
        .collect();

    let error_detail = error.map(|msg| rpc::Status {
        code: tonic::Code::InvalidArgument.into(),
        message: msg.to_string(),
        ..Default::default()
    });

    DeltaDiscoveryRequest {
        type_url: rtype.type_url().to_string(),
        response_nonce: response_nonce.to_string(),
        resource_names_subscribe,
        resource_names_unsubscribe,
        initial_resource_versions,
        error_detail,
        ..Default::default()
    }
}

macro_rules! resp {
    (n = $nonce:expr, ty = $rtype:expr, remove = $remove:expr $(,)*) => {
        crate::xds::test::empty_delta_discovery_response($nonce, None, $rtype, $remove)
    };
    (n = $nonce:expr, add = $add:expr, remove = $remove:expr $(,)*) => {
        crate::xds::test::delta_discovery_response($nonce, None, $add, $remove)
    };
}

pub(crate) use resp;

pub fn empty_delta_discovery_response(
    nonce: &'static str,
    version: Option<&'static str>,
    rtype: ResourceType,
    removed_resources: Vec<&'static str>,
) -> DeltaDiscoveryResponse {
    let type_url = rtype.type_url().to_string();
    let system_version_info = version.map(|s| s.to_string()).unwrap_or_default();
    let removed_resources = removed_resources
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    DeltaDiscoveryResponse {
        type_url,
        system_version_info,
        nonce: nonce.to_string(),
        removed_resources,
        ..Default::default()
    }
}

pub fn delta_discovery_response(
    nonce: &'static str,
    version: Option<&'static str>,
    resources: Vec<xds_discovery::Resource>,
    removed_resources: Vec<&'static str>,
) -> DeltaDiscoveryResponse {
    let type_url = resources.first().and_then(resource_type_url).unwrap();
    let system_version_info = version.map(|s| s.to_string()).unwrap_or_default();
    let removed_resources = removed_resources
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    DeltaDiscoveryResponse {
        type_url,
        system_version_info,
        nonce: nonce.to_string(),
        resources,
        removed_resources,
        ..Default::default()
    }
}

#[inline]
fn resource_type_url(resource: &xds_discovery::Resource) -> Option<String> {
    resource.resource.as_ref().map(|r| r.type_url.clone())
}

pub fn xds_resource<T: prost::Name>(
    name: String,
    version: String,
    xds: T,
) -> xds_discovery::Resource {
    xds_discovery::Resource {
        name,
        version,
        resource: Some(protobuf::Any::from_msg(&xds).unwrap()),
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
    route_name: &'static str,
    virtual_hosts: Vec<xds_route::VirtualHost>,
) -> xds_listener::Listener {
    use xds_http::{http_connection_manager::RouteSpecifier, http_filter::ConfigType};

    let http_router_filter = Router::default();
    let route_specifier = RouteSpecifier::RouteConfig(xds_route::RouteConfiguration {
        name: route_name.to_string(),
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
    match_query: Option<(&str, &str)>,
    cluster_name: &str,
) -> xds_route::Route {
    let mut route_match = xds_route::RouteMatch {
        ..Default::default()
    };

    route_match.path_specifier = match path {
        Some(path) => Some(xds_route::route_match::PathSpecifier::Path(
            path.to_string(),
        )),
        None => Some(xds_route::route_match::PathSpecifier::Prefix(
            "".to_string(),
        )),
    };

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

    if let Some((name, value)) = match_query {
        let query_matcher = xds_route::QueryParameterMatcher {
            name: name.to_string(),
            query_parameter_match_specifier: Some(
                xds_route::query_parameter_matcher::QueryParameterMatchSpecifier::StringMatch(
                    xds_matcher::StringMatcher {
                        ignore_case: false,
                        match_pattern: Some(xds_matcher::string_matcher::MatchPattern::Exact(
                            value.to_string(),
                        )),
                    },
                ),
            ),
        };
        route_match.query_parameters = vec![query_matcher];
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

pub fn eds_cluster(
    name: &'static str,
    lb_policy: Option<xds_cluster::cluster::LbPolicy>,
) -> xds_cluster::Cluster {
    use xds_cluster::cluster::ClusterDiscoveryType;
    use xds_cluster::cluster::DiscoveryType;
    use xds_cluster::cluster::EdsClusterConfig;

    let cluster_discovery_type = Some(ClusterDiscoveryType::Type(DiscoveryType::Eds.into()));
    let eds_cluster_config = Some(EdsClusterConfig {
        eds_config: Some(ads_config_source()),
        service_name: name.to_string(),
    });
    let mut cluster = xds_cluster::Cluster {
        name: name.to_string(),
        cluster_discovery_type,
        eds_cluster_config,
        ..Default::default()
    };
    if let Some(lb_policy) = lb_policy {
        cluster.lb_policy = lb_policy.into();
    }
    cluster
}

pub fn logical_dns_cluster(
    name: &str,
    hostname: &str,
    port: u16,
    lb_policy: Option<xds_cluster::cluster::LbPolicy>,
) -> xds_cluster::Cluster {
    use xds_cluster::cluster::ClusterDiscoveryType;
    use xds_cluster::cluster::DiscoveryType;

    let cluster_discovery_type = Some(ClusterDiscoveryType::Type(DiscoveryType::LogicalDns.into()));
    let host_identifier = Some(xds_endpoint::lb_endpoint::HostIdentifier::Endpoint(
        xds_endpoint::Endpoint {
            address: Some(to_xds_address(hostname, port)),
            ..Default::default()
        },
    ));
    let endpoints = vec![xds_endpoint::LocalityLbEndpoints {
        lb_endpoints: vec![xds_endpoint::LbEndpoint {
            host_identifier,
            ..Default::default()
        }],
        ..Default::default()
    }];
    let load_assignment = Some(xds_endpoint::ClusterLoadAssignment {
        endpoints,
        ..Default::default()
    });

    let mut cluster = xds_cluster::Cluster {
        name: name.to_string(),
        cluster_discovery_type,
        load_assignment,
        ..Default::default()
    };
    if let Some(lb_policy) = lb_policy {
        cluster.lb_policy = lb_policy.into();
    }
    cluster
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

fn to_xds_address(hostname: &str, port: u16) -> xds_core::Address {
    let socket_address = xds_core::SocketAddress {
        address: hostname.to_string(),
        port_specifier: Some(xds_core::socket_address::PortSpecifier::PortValue(
            port as u32,
        )),
        ..Default::default()
    };

    xds_core::Address {
        address: Some(xds_core::address::Address::SocketAddress(socket_address)),
    }
}
