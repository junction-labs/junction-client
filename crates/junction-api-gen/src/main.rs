mod python;

use junction_typeinfo::TypeInfo as _;

/// Code generation tools for cross-language APIs that can't (or shouldn't) be
/// expressed through FFI.
///
/// This is not a general purpose tool.
fn main() {
    let items = vec![
        junction_api_types::shared::Fraction::item(),
        junction_api_types::shared::WeightedTarget::item(),
        junction_api_types::shared::Target::item(),
        junction_api_types::shared::SessionAffinityHashParam::item(),
        junction_api_types::http::RouteTimeouts::item(),
        junction_api_types::http::RouteRetry::item(),
        junction_api_types::http::HeaderValue::item(),
        junction_api_types::http::HeaderMatch::item(),
        junction_api_types::http::QueryParamMatch::item(),
        junction_api_types::http::PathMatch::item(),
        junction_api_types::http::RouteMatch::item(),
        // NOTE: filters are currently hidden from the docs, don't generate
        //       typing info for them.
        //
        // junction_api_types::http::HeaderFilter::item(),
        // junction_api_types::http::RequestMirrorFilter::item(),
        // junction_api_types::http::PathModifier::item(),
        // junction_api_types::http::RequestRedirectFilter::item(),
        // junction_api_types::http::UrlRewriteFilter::item(),
        // junction_api_types::http::RouteFilter::item(),
        junction_api_types::shared::SessionAffinity::item(),
        junction_api_types::http::RouteRule::item(),
        junction_api_types::http::Route::item(),
        junction_api_types::backend::LbPolicy::item(),
        junction_api_types::backend::Backend::item(),
    ];

    let mut buf = String::with_capacity(4 * 1024);
    python::generate(&mut buf, items).unwrap();
    println!("{buf}");
}
