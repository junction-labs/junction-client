use xds_api::pb::envoy::extensions::filters::{
    http::router::v3 as xds_router, network::http_connection_manager::v3 as xds_http,
};
use xds_api::pb::{
    envoy::{
        config::{
            listener::v3 as xds_listener,
            route::v3::{self as xds_route},
        },
        service::discovery::v3::{self as xds_discovery},
    },
    google::protobuf,
};

fn main() {
    let xds = &[
        api_listener("http", "httpbin.org", 80),
        api_listener("https", "httpbin.org", 443),
    ];
    let headers = http::HeaderMap::new();

    for (scheme, port) in [("http", 80), ("https", 443)] {
        let httpbin_org = format!("{scheme}://httpbin.org").parse().unwrap();
        assert_eq!(
            junction_core::check_route(xds, None, &http::Method::GET, &httpbin_org, &headers)
                .unwrap(),
            format!("httpbin.org:{port}")
        );
    }

    let example_com = "http://example.com".parse().unwrap();
    assert!(
        junction_core::check_route(xds, None, &http::Method::GET, &example_com, &headers).is_err()
    );
}

fn api_listener(name: &str, hostname: &str, port: u16) -> xds_discovery::Resource {
    use xds_http::{http_connection_manager::RouteSpecifier, http_filter::ConfigType};

    let route = xds_route::Route {
        r#match: Some(xds_route::RouteMatch {
            path_specifier: Some(xds_route::route_match::PathSpecifier::Prefix(
                "".to_string(),
            )),
            ..Default::default()
        }),
        action: Some(xds_route::route::Action::Route(xds_route::RouteAction {
            cluster_specifier: Some(xds_route::route_action::ClusterSpecifier::Cluster(format!(
                "{hostname}:{port}"
            ))),
            ..Default::default()
        })),
        ..Default::default()
    };

    let http_connection_manager = xds_http::HttpConnectionManager {
        route_specifier: Some(RouteSpecifier::RouteConfig(xds_route::RouteConfiguration {
            name: name.to_string(),
            virtual_hosts: vec![xds_route::VirtualHost {
                name: name.to_string(),
                domains: vec![hostname.to_string().to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        })),
        http_filters: vec![xds_http::HttpFilter {
            name: name.to_string(),
            config_type: Some(ConfigType::TypedConfig(
                protobuf::Any::from_msg(&xds_router::Router::default()).unwrap(),
            )),
            ..Default::default()
        }],
        ..Default::default()
    };

    let listener = xds_listener::Listener {
        name: format!("{hostname}:{port}"),
        api_listener: Some(xds_listener::ApiListener {
            api_listener: Some(protobuf::Any::from_msg(&http_connection_manager).unwrap()),
        }),
        ..Default::default()
    };

    let resource = Some(protobuf::Any::from_msg(&listener).unwrap());
    xds_discovery::Resource {
        name: listener.name,
        resource,
        ..Default::default()
    }
}
