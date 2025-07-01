use std::time::{Duration, SystemTime};

use http::Method;
use junction_core::{Client, Endpoint};
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

    for _ in 0..10 {
        let endpoint = client
            .resolve_http(
                &Method::GET,
                &"http://nginx.default.svc.cluster.local".parse().unwrap(),
                &http::HeaderMap::new(),
            )
            .await
            .unwrap();

        print_trace(&endpoint);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn print_trace(endpoint: &Endpoint) {
    eprintln!("addr={}", endpoint.addr());
    let trace = endpoint.trace();
    let mut last_time = trace.start();
    for event in trace.events() {
        let event_time = event.time();
        let since_start = elapsed_f64(event_time, trace.start());
        let since_last = elapsed_f64(event_time, last_time);
        last_time = event_time;

        eprintln!(
            "  at={since_start:.06} since_last={since_last:.06} kind={kind}",
            kind = event.kind(),
        )
    }
}

#[inline]
fn elapsed_f64(t: SystemTime, start: SystemTime) -> f64 {
    t.duration_since(start).unwrap_or_default().as_secs_f64()
}
