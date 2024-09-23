pub mod backend;
pub mod crd;
pub mod http;
pub mod shared;

macro_rules! value_or_default {
    ($value:expr, $default:expr) => {
        $value.as_ref().map(|v| v.value).unwrap_or($default)
    };
}

pub(crate) use value_or_default;
