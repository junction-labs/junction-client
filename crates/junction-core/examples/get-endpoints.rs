use http::HeaderValue;
use junction_api_types::{
    backend::{Backend, LbPolicy},
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
    tokio::spawn(client.csds_server(8009));

    let nginx = Attachment::Service(ServiceAttachment {
        name: "nginx".to_string(),
        namespace: Some("default".to_string()),
        port: Some(80),
    });
    let nginx_staging = Attachment::Service(ServiceAttachment {
        name: "nginx-staging".to_string(),
        namespace: Some("default".to_string()),
        port: Some(80),
    });

    let default_routes = vec![Route {
        attachment: nginx.clone(),
        rules: vec![
            RouteRule {
                matches: vec![RouteMatch {
                    headers: vec![HeaderMatch {
                        name: "x-demo-staging".to_string(),
                        matches: StringMatch::RegularExpression {
                            value: Regex::from_str(".*").unwrap(),
                        },
                    }],
                    ..Default::default()
                }],
                backends: vec![WeightedBackend {
                    attachment: nginx_staging.clone(),
                    weight: 1,
                }],
                ..Default::default()
            },
            RouteRule {
                backends: vec![WeightedBackend {
                    attachment: nginx.clone(),
                    weight: 1,
                }],
                ..Default::default()
            },
        ],
    }];
    let default_backends = vec![
        Backend {
            attachment: nginx,
            lb: LbPolicy::Unspecified,
        },
        Backend {
            attachment: nginx_staging,
            lb: LbPolicy::Unspecified,
        },
    ];
    let mut client = client
        .with_defaults(default_routes, default_backends)
        .unwrap();

    let url: http::Uri = "https://nginx.default.svc.cluster.local".parse().unwrap();
    let prod_headers = http::HeaderMap::new();
    let staging_headers = {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-demo-staging", HeaderValue::from_static("true"));
        headers
    };

    loop {
        let prod_endpoints =
            client.resolve_endpoints(&http::Method::GET, url.clone(), &prod_headers);
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
