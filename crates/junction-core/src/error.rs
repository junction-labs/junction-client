use std::borrow::Cow;

use junction_api::{backend::BackendId, Name};

/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

/// An error when using the Junction client.
#[derive(Debug, thiserror::Error)]
#[error("{inner}")]
pub struct Error {
    // boxed to keep the size of the error down. this apparently has a large
    // effect on the performance of calls to functions that return
    // Result<_, Error>.
    //
    // https://rust-lang.github.io/rust-clippy/master/index.html#result_large_err
    // https://docs.rs/serde_json/latest/src/serde_json/error.rs.html#15-20
    inner: Box<ErrorImpl>,
}

impl Error {
    /// Returns `true` if this is a temporary error.
    ///
    /// Temporary errors may occur because of a network timeout or because of
    /// lag fetching a configuration from a Junction server.
    pub fn is_temporary(&self) -> bool {
        matches!(*self.inner, ErrorImpl::NoReachableEndpoints { .. })
    }
}

impl Error {
    // timeouts

    pub(crate) fn timed_out(message: &'static str) -> Self {
        let inner = ErrorImpl::TimedOut(Cow::from(message));
        Self {
            inner: Box::new(inner),
        }
    }

    // url problems

    pub(crate) fn into_invalid_url(message: String) -> Self {
        let inner = ErrorImpl::InvalidUrl(Cow::Owned(message));
        Self {
            inner: Box::new(inner),
        }
    }

    pub(crate) fn invalid_url(message: &'static str) -> Self {
        let inner = ErrorImpl::InvalidUrl(Cow::Borrowed(message));
        Self {
            inner: Box::new(inner),
        }
    }

    // route problems

    pub(crate) fn no_route_matched(authority: String) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoRouteMatched { authority }),
        }
    }

    pub(crate) fn no_rule_matched(route: Name) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoRuleMatched { route }),
        }
    }

    pub(crate) fn invalid_route(message: &'static str, id: Name, rule: usize) -> Self {
        Self {
            inner: Box::new(ErrorImpl::InvalidRoute { id, message, rule }),
        }
    }

    // backend problems

    pub(crate) fn no_backend(route: Name, rule: Option<usize>, backend: BackendId) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoBackend {
                route,
                rule,
                backend,
            }),
        }
    }

    pub(crate) fn no_reachable_endpoints(route: Name, backend: BackendId) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoReachableEndpoints { route, backend }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ErrorImpl {
    #[error("timed out: {0}")]
    TimedOut(Cow<'static, str>),

    #[error("invalid url: {0}")]
    InvalidUrl(Cow<'static, str>),

    #[error("invalid route configuration")]
    InvalidRoute {
        message: &'static str,
        id: Name,
        rule: usize,
    },

    #[error("no route matched: '{authority}'")]
    NoRouteMatched { authority: String },

    #[error("using route '{route}': no routing rules matched the request")]
    NoRuleMatched { route: Name },

    #[error("{route}: backend not found: {backend}")]
    NoBackend {
        route: Name,
        rule: Option<usize>,
        backend: BackendId,
    },

    #[error("{backend}: no reachable endpoints")]
    NoReachableEndpoints { route: Name, backend: BackendId },
}
