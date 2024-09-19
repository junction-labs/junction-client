pub mod gateway_api;
pub mod jct_backend_policy;
pub mod jct_http_retry_policy;
pub mod jct_http_route_policy;
pub mod jct_http_session_affinity_policy;
pub mod jct_lb_policy;
pub mod jct_parent_ref;

macro_rules! value_or_default {
    ($value:expr, $default:expr) => {
        $value.as_ref().map(|v| v.value).unwrap_or($default)
    };
}
pub(crate) use value_or_default;
