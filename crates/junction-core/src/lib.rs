//! An XDS-compatible HTTP Client.

/// everything needs urls and error
mod error;
mod url;

use std::sync::Arc;

pub use crate::error::{Error, Result};
pub use crate::url::Url;

// rand is useful
pub(crate) mod rand;

// config needs to be public internally but should be exposed extremely
// sparingly.
mod config;

pub use config::endpoints::{Endpoint, EndpointAddress};

// only the client needs to be exported.
mod client;
mod xds;

pub use client::Client;
use config::StaticConfig;
use junction_api::http::Route;
use junction_api::shared::Target;
pub use xds::{ResourceVersion, XdsConfig};

/// Check route resolution.
///
/// Resolves a route against a table of routes, returning the chosen [Route],
/// the index of the rule that matched, and the [Target] selected from the
/// route.
///
/// [Client::resolve_endpoints] resolves routes in exactly the same way as this
/// function. Use it to test routing configuration without requiring a full
/// client or connecting to a control plane.
pub fn check_route(
    routes: Vec<Route>,
    method: &http::Method,
    url: crate::Url,
    headers: &http::HeaderMap,
) -> Result<(Route, usize, Target)> {
    let config = StaticConfig::new(routes, Vec::new());
    let (_, resolved) =
        client::resolve_routes(&StaticConfig::default(), &config, method, url, headers)
            .map_err(|(_, e)| e)?;

    // Drop the other copies of the resolved routes and unwrap the Arc.
    //
    // safety: we completely own these routes, and the only copies should be
    // the one returned and the one in `config` we're dropping here.
    std::mem::drop(config);
    let route = Arc::into_inner(resolved.route).unwrap();
    Ok((route, resolved.rule, resolved.backend))
}
