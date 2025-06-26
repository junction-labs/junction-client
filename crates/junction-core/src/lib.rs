//! The core implementation for Junction - an xDS dynamically-configurable API load-balancer library.
//!
//! * [Getting Started](https://docs.junctionlabs.io/getting-started/rust)

mod error;
mod trace;
mod url;
pub use crate::error::{Error, Result};
pub use crate::url::Url;

pub(crate) mod hash;
pub(crate) mod rand;

mod endpoints;
pub use endpoints::Endpoint;

mod client;
mod dns;
mod xds;

pub use client::{Client, HttpRequest, HttpResult, SearchConfig, SelectedEndpoint};
use futures::FutureExt;
use trace::Trace;
pub use xds::{IntoXds, XdsConfig};

/// Check route resolution.
///
/// Resolves a request against a static set of xDS configuration, returning the
/// name of the Cluster that was selected by this route.
///
/// Use this function to test routing configuration without requiring a full
/// client or a live connection to a control plane. For route resolution against,
/// a live control plane, see [Client::resolve_http].
pub fn check_route<T: IntoXds>(
    resources: impl IntoIterator<Item = T>,
    search_config: Option<&SearchConfig>,
    method: &http::Method,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Result<String> {
    let request = client::HttpRequest::from_parts(method, url, headers);
    let client = xds::StaticCache::with_xds(resources.into_iter()).unwrap();
    let search_config = search_config.cloned().unwrap_or_default();

    // resolve_routes is async but we know that with StaticConfig, fetching
    // config should NEVER block. now-or-never just calls Poll with a noop
    // waker and unwraps the result ASAP.
    let resolved = client::resolve_route(&client, &search_config, Trace::new(), request, None)
        .now_or_never()
        .expect(
            "check_route yielded unexpectedly. this is a bug in Junction, please file an issue",
        )?;

    Ok(resolved.cluster.to_string())
}
