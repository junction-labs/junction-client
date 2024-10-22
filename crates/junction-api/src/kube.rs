//! Types and re-exports for converting Junction types to Kubernetes objects.
//!
//! When the `kube` feature of this crate is active,
//! [Route](crate::http::Route)s and [Backend](crate::backend::Backend)s can be
//! converted into Kubernetes API types. To help avoid dependency conflicts with
//! different versions of [`k8s-openapi`](https://crates.io/crates/k8s-openapi)
//! and [`gateway-api`](https://crates.io/crates/gateway-api), this crate
//! re-exports its versions of those dependencies.
//!
//! ```no_run
//! // Import re-exported deps to make sure versions match
//! use junction_api::kube::k8s_openapi;
//!
//! // Use the re-exported version as normal
//! use k8s_openapi::api::core::v1::PodSpec;
//! let spec = PodSpec::default();
//! ```
//!
//! This crate does not set a [`k8s-openapi` version feature](https://docs.rs/k8s-openapi/latest/k8s_openapi/#crate-features),
//! application authors (but not library authors!) who depend on `junction-api`
//! with the `kube` feature enabled will still need to include `k8s-openapi` as
//! a direct dependency and set the appropriate cargo feature.

mod backend;
mod http;

pub use gateway_api;
pub use k8s_openapi;
