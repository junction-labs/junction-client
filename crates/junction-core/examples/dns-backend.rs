use std::{env, time::Duration};

use http::Method;
use junction_api::{
    backend::{Backend, LbPolicy},
    Service,
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

    let httpbin = Service::dns("httpbin.org").unwrap();

    let backends = vec![
        Backend {
            id: httpbin.as_backend_id(80),
            lb: LbPolicy::RoundRobin,
        },
        Backend {
            id: httpbin.as_backend_id(443),
            lb: LbPolicy::RoundRobin,
        },
    ];

    let client = client.with_static_config(vec![], backends);

    eprintln!("{:?}", client.dump_routes());

    let http_url: junction_core::Url = "http://httpbin.org".parse().unwrap();
    let https_url: junction_core::Url = "https://httpbin.org".parse().unwrap();
    let headers = http::HeaderMap::new();

    loop {
        match client.resolve_http(&Method::GET, &http_url, &headers).await {
            Ok(endpoint) => {
                eprintln!(" http: {:>15}", &endpoint.addr());
            }
            Err(e) => eprintln!("http: something went wrong: {e}"),
        }
        match client
            .resolve_http(&Method::GET, &https_url, &headers)
            .await
        {
            Ok(endpoint) => {
                eprintln!("https: {:>15}", &endpoint.addr());
            }
            Err(e) => eprintln!("https: something went wrong: {e}"),
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
