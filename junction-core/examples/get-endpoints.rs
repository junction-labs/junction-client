use http::HeaderValue;
use junction_core::{
    Client, HeaderMatcher, Route, RouteMatcher, RouteRule, RouteTarget, StringMatcher,
};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let default_routes = vec![Route {
        domains: vec!["nginx.default.svc.cluster.local".to_string()],
        rules: vec![
            RouteRule {
                timeout: None,
                retry_policy: None,
                matches: vec![RouteMatcher {
                    path: StringMatcher::Any,
                    headers: vec![HeaderMatcher {
                        name: "x-demo-staging".to_string(),
                        value: StringMatcher::Any,
                    }],
                }],
                hash_policies: vec![],
                target: RouteTarget::Cluster("default/nginx-staging/cluster".to_string()),
            },
            RouteRule {
                timeout: None,
                retry_policy: None,
                matches: vec![RouteMatcher {
                    path: StringMatcher::Any,
                    headers: vec![],
                }],
                hash_policies: vec![],
                target: RouteTarget::Cluster("default/nginx/cluster".to_string()),
            },
        ],
    }];
    let mut client = Client::build(
        "http://127.0.0.1:8008".to_string(),
        "example-client".to_string(),
        "example-cluster".to_string(),
        default_routes,
    )
    .await
    .unwrap();

    tokio::spawn(client.config_server(8009));

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
