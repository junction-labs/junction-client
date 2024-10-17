//! Junction API Configuration.
//!
//! These types let you express routing and load balancing configuration as data
//! structures. The `kube` and `xds` features of this crate let you convert
//! Junction configuration to Kubernetes objects and xDS resources respectively.
//!
//! Use this crate directly if you're looking to build and export configuration.
//! Use the `junction-client` crate if you want to make requests and resolve
//! addresses with Junction.

mod error;
pub use error::Error;

pub mod backend;
pub mod http;
pub mod shared;

#[cfg(feature = "xds")]
mod xds;

#[cfg(feature = "kube")]
mod kube;

macro_rules! value_or_default {
    ($value:expr, $default:expr) => {
        $value.as_ref().map(|v| v.value).unwrap_or($default)
    };
}

pub(crate) use value_or_default;
