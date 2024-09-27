use http::HeaderValue;
use junction_api_types::{
    http::*,
    shared::{Attachment, Regex, ServiceAttachment, StringMatch, WeightedBackend},
};
use junction_core::Client;
use std::{str::FromStr, time::Duration};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let client = Client::build(
        "http://127.0.0.1:8008".to_string(),
        "example-client".to_string(),
        "example-cluster".to_string(),
    )
    .await
    .unwrap();
    tokio::spawn(client.config_server(8009));

    let default_routes = vec![Route {
        attachment: Attachment::Service(ServiceAttachment {
            name: "nginx".to_string(),
            namespace: Some("default".to_string()),
            port: None,
        }),
        rules: vec![
            RouteRule {
                matches: vec![RouteMatch {
                    headers: vec![HeaderMatch {
                        name: "x-demo-staging".to_string(),
                        value_matcher: StringMatch::RegularExpression {
                            value: Regex::from_str(".*").unwrap(),
                        },
                    }],
                    path: None,
                    method: None,
                    query_params: vec![],
                }],
                filters: vec![],
                timeouts: None,
                retry_policy: None,
                session_affinity: None,
                backends: vec![WeightedBackend {
                    attachment: Attachment::from_cluster_xds_name("default/nginx-staging/cluster")
                        .unwrap(),
                    weight: 1,
                }],
            },
            RouteRule {
                matches: vec![],
                filters: vec![],
                timeouts: None,
                retry_policy: None,
                session_affinity: None,
                backends: vec![WeightedBackend {
                    attachment: Attachment::from_cluster_xds_name("default/nginx/cluster").unwrap(),
                    weight: 1,
                }],
            },
        ],
    }];
    let default_backends = vec![];
    let mut client = client.with_defaults(default_routes, default_backends);

    let url: http::Uri = "https://nginx.default.svc.cluster.local".parse().unwrap();
    let headers = http::HeaderMap::new();
    let mut staging_headers = http::HeaderMap::new();
    staging_headers.insert("x-demo-staging", HeaderValue::from_static("true"));

    loop {
        let prod_endpoints = client.resolve_endpoints(&http::Method::GET, url.clone(), &headers);
        let staging_endpoints =
            client.resolve_endpoints(&http::Method::GET, url.clone(), &staging_headers);

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
