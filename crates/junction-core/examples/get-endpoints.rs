use http::HeaderValue;
use junction_api::{
    backend::{Backend, LbPolicy},
    http::{BackendRef, HeaderMatch, Route, RouteMatch, RouteRule},
    Name, Regex, Service,
};
use junction_core::Client;
use std::{env, str::FromStr, time::Duration};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let server_addr =
        env::var("JUNCTION_ADS_SERVER").unwrap_or("http://127.0.0.1:8008".to_string());

    let client = Client::build(
        server_addr,
        "example-client".to_string(),
        "example-cluster".to_string(),
    )
    .await
    .unwrap();

    // spawn a CSDS server that allows inspecting xDS over gRPC while the client
    // is running
    tokio::spawn(client.clone().csds_server(8009));

    let nginx = Service::kube("default", "nginx").unwrap();
    let nginx_staging = Service::kube("default", "nginx-staging").unwrap();

    let routes = vec![Route {
        id: Name::from_static("nginx"),
        hostnames: vec![nginx.hostname().into()],
        ports: vec![],
        tags: Default::default(),
        rules: vec![
            RouteRule {
                matches: vec![RouteMatch {
                    headers: vec![HeaderMatch::RegularExpression {
                        name: "x-demo-staging".to_string(),
                        value: Regex::from_str(".*").unwrap(),
                    }],
                    ..Default::default()
                }],
                backends: vec![BackendRef {
                    service: nginx_staging.clone(),
                    port: Some(80),
                    weight: 1,
                }],
                ..Default::default()
            },
            RouteRule {
                backends: vec![BackendRef {
                    service: nginx.clone(),
                    port: Some(80),
                    weight: 1,
                }],
                ..Default::default()
            },
        ],
    }];
    let backends = vec![
        Backend {
            id: nginx.as_backend_id(80),
            lb: LbPolicy::Unspecified,
        },
        Backend {
            id: nginx_staging.as_backend_id(80),
            lb: LbPolicy::Unspecified,
        },
    ];
    let mut client = client.with_static_config(routes, backends);

    let url: junction_core::Url = "https://nginx.default.svc.cluster.local".parse().unwrap();
    let prod_headers = http::HeaderMap::new();
    let staging_headers = {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-demo-staging", HeaderValue::from_static("true"));
        headers
    };

    loop {
        let prod_endpoints = client
            .resolve_http(&http::Method::GET, &url, &prod_headers)
            .await;
        let staging_endpoints = client
            .resolve_http(&http::Method::GET, &url, &staging_headers)
            .await;

        let mut error = false;
        if let Err(e) = &prod_endpoints {
            eprintln!("error: prod: {e:?}");
            error = true;
        }
        if let Err(e) = &staging_endpoints {
            eprintln!("error: staging: {e:?}");
            error = true;
        }

        if error {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        let prod = prod_endpoints.unwrap();
        let staging = staging_endpoints.unwrap();
        println!("prod={:<20} staging={:<20}", prod.addr(), staging.addr());

        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
}
