use std::{collections::BTreeMap, env, time::Duration};

use http::Method;
use junction_api::{
    backend::{Backend, LbPolicy},
    http::Route,
    Target,
};
use junction_core::Client;
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

    let httpbin = Target::dns("httpbin.org").unwrap();

    let default_route = Route {
        vhost: httpbin.clone().into_vhost(None),
        tags: BTreeMap::new(),
        rules: vec![],
    };
    let http_backend = Backend {
        id: httpbin.clone().into_backend(80),
        lb: LbPolicy::RoundRobin,
    };
    let https_backend = Backend {
        id: httpbin.clone().into_backend(443),
        lb: LbPolicy::RoundRobin,
    };

    let mut client = client
        .with_defaults(vec![default_route], vec![http_backend, https_backend])
        .unwrap();

    let http_url: junction_core::Url = "http://httpbin.org".parse().unwrap();
    let https_url: junction_core::Url = "https://httpbin.org".parse().unwrap();
    let headers = http::HeaderMap::new();

    loop {
        match client.resolve_http(&Method::GET, &http_url, &headers) {
            Ok(endpoints) => {
                eprintln!("http: {}", &endpoints[0].address);
            }
            Err(e) => eprintln!("oops, something went wrong: {e:?}"),
        }
        match client.resolve_http(&Method::GET, &https_url, &headers) {
            Ok(endpoints) => {
                eprintln!("https: {}", &endpoints[0].address);
            }
            Err(e) => eprintln!("oops, something went wrong: {e:?}"),
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
