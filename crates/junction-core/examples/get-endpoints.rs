use http::HeaderValue;
use junction_api::{
    backend::{Backend, LbPolicy},
    http::{HeaderMatch, Route, RouteMatch, RouteRule, WeightedBackend},
    Regex, Target,
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
    tokio::spawn(client.csds_server(8009));

    let nginx = Target::kube_service("default", "nginx").unwrap();
    let nginx_staging = Target::kube_service("default", "nginx-staging").unwrap();

    let default_routes = vec![Route {
        vhost: nginx.clone().into_vhost(None),
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
                backends: vec![WeightedBackend {
                    backend: nginx_staging.clone().into_backend(80),
                    weight: 1,
                }],
                ..Default::default()
            },
            RouteRule {
                backends: vec![WeightedBackend {
                    backend: nginx.clone().into_backend(80),
                    weight: 1,
                }],
                ..Default::default()
            },
        ],
    }];
    let default_backends = vec![
        Backend {
            id: nginx.into_backend(80),
            lb: LbPolicy::Unspecified,
        },
        Backend {
            id: nginx_staging.into_backend(80),
            lb: LbPolicy::Unspecified,
        },
    ];
    let mut client = client
        .with_defaults(default_routes, default_backends)
        .unwrap();

    let url: junction_core::Url = "https://nginx.default.svc.cluster.local".parse().unwrap();
    let prod_headers = http::HeaderMap::new();
    let staging_headers = {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-demo-staging", HeaderValue::from_static("true"));
        headers
    };

    loop {
        let prod_endpoints = client.resolve_http(&http::Method::GET, &url, &prod_headers);
        let staging_endpoints = client.resolve_http(&http::Method::GET, &url, &staging_headers);

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

        let prod_endpoints = prod_endpoints.unwrap();
        let staging_endpoints = staging_endpoints.unwrap();
        let prod = prod_endpoints.first().unwrap();
        let staging = staging_endpoints.first().unwrap();

        println!("prod={:<20} staging={:<20}", prod.address, staging.address);

        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
}
