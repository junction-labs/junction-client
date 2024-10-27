mod python;

use junction_typeinfo::TypeInfo as _;

/// Code generation tools for cross-language APIs that can't (or shouldn't) be
/// expressed through FFI.
///
/// This is not a general purpose tool.
fn main() {
    let items = vec![
        junction_api::Target::item(),
        junction_api::RouteTarget::item(),
        junction_api::BackendTarget::item(),
        junction_api::Fraction::item(),
        junction_api::http::WeightedTarget::item(),
        junction_api::http::RouteTimeouts::item(),
        junction_api::http::RouteRetry::item(),
        junction_api::http::HeaderValue::item(),
        junction_api::http::HeaderMatch::item(),
        junction_api::http::QueryParamMatch::item(),
        junction_api::http::PathMatch::item(),
        junction_api::http::RouteMatch::item(),
        // NOTE: filters are currently hidden from the docs, don't generate
        //       typing info for them.
        //
        // junction_api::http::HeaderFilter::item(),
        // junction_api::http::RequestMirrorFilter::item(),
        // junction_api::http::PathModifier::item(),
        // junction_api::http::RequestRedirectFilter::item(),
        // junction_api::http::UrlRewriteFilter::item(),
        // junction_api::http::RouteFilter::item(),
        junction_api::http::RouteRule::item(),
        junction_api::http::Route::item(),
        junction_api::backend::SessionAffinityHashParam::item(),
        junction_api::backend::SessionAffinity::item(),
        junction_api::backend::LbPolicy::item(),
        junction_api::backend::Backend::item(),
    ];

    let mut buf = String::with_capacity(4 * 1024);
    python::generate(&mut buf, items).unwrap();
    println!("{buf}");
}
