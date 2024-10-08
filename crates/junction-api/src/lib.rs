mod error;

pub use error::Error;

pub mod backend;
pub mod http;
pub mod shared;

#[cfg(feature = "xds")]
pub mod xds;

#[cfg(feature = "kube")]
pub mod kube;

macro_rules! value_or_default {
    ($value:expr, $default:expr) => {
        $value.as_ref().map(|v| v.value).unwrap_or($default)
    };
}

pub(crate) use value_or_default;
