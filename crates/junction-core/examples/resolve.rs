use http::Method;
use junction_core::Client;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let server_addr =
        std::env::var("JUNCTION_ADS_SERVER").unwrap_or("http://127.0.0.1:8008".to_string());

    let client = Client::builder("node".to_string(), "cluster".to_string())
        .build(server_addr)
        .await
        .unwrap();

    let endpoint = client
        .resolve_http(
            &Method::GET,
            &"http://httpbin.org".parse().unwrap(),
            &http::HeaderMap::new(),
        )
        .await
        .unwrap();

    eprintln!("addr={}", endpoint.addr());
    endpoint.print_trace();
}
