//! An XDS-compatible HTTP Client.

/// everything needs urls and error
mod error;
mod url;

pub use crate::error::{Error, Result};
pub use crate::url::Url;

// rand is useful
pub(crate) mod rand;

// config needs to be public internally but should be exposed extremely
// sparingly.
mod config;

pub use config::{
    endpoints::{Endpoint, EndpointAddress, RetryPolicy},
    route::{HeaderMatcher, Route, RouteMatcher, RouteRule, RouteTarget, StringMatcher},
};

pub(crate) use config::load_balancer::RequestContext;

// only the client needs to be exported.
mod client;
mod xds;

pub use client::Client;
