mod python;

use junction_typeinfo::TypeInfo as _;

/// Code generation tools for cross-language APIs that can't (or shouldn't) be
/// expressed through FFI.
///
/// This is not a general purpose tool.
fn main() {
    let items = vec![
        junction_api_types::shared::Fraction::item(),
        junction_api_types::shared::WeightedBackend::item(),
        junction_api_types::shared::Attachment::item(),
        junction_api_types::shared::SessionAffinityHashParam::item(),
        junction_api_types::http::RouteTimeouts::item(),
        junction_api_types::http::RouteRetryPolicy::item(),
        junction_api_types::http::HeaderValue::item(),
        junction_api_types::http::HeaderMatch::item(),
        junction_api_types::http::QueryParamMatch::item(),
        junction_api_types::http::PathMatch::item(),
        junction_api_types::http::RouteMatch::item(),
        junction_api_types::http::RequestHeaderFilter::item(),
        junction_api_types::http::RequestMirrorFilter::item(),
        junction_api_types::http::PathModifier::item(),
        junction_api_types::http::RequestRedirectFilter::item(),
        junction_api_types::http::UrlRewriteFilter::item(),
        junction_api_types::http::RouteFilter::item(),
        junction_api_types::shared::SessionAffinityPolicy::item(),
        junction_api_types::http::RouteRule::item(),
        junction_api_types::http::Route::item(),
        junction_api_types::backend::LbPolicy::item(),
        junction_api_types::backend::Backend::item(),
    ];

    let mut buf = String::with_capacity(4 * 1024);
    python::generate(&mut buf, items).unwrap();
    println!("{buf}");
}
