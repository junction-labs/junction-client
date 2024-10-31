use std::borrow::Cow;

use junction_api::{BackendId, VirtualHost};

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
        matches!(
            *self.inner,
            ErrorImpl::NoRouteMatched { .. }
                | ErrorImpl::NoBackend { .. }
                | ErrorImpl::NoRuleMatched { .. }
                | ErrorImpl::NoReachableEndpoints { .. }
        )
    }
}

impl Error {
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

    pub(crate) fn no_route_matched(routes: Vec<VirtualHost>) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoRouteMatched { routes }),
        }
    }

    pub(crate) fn no_rule_matched(route: VirtualHost) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoRuleMatched { route }),
        }
    }

    pub(crate) fn invalid_route(message: &'static str, vhost: VirtualHost, rule: usize) -> Self {
        Self {
            inner: Box::new(ErrorImpl::InvalidRoute {
                message,
                vhost,
                rule,
            }),
        }
    }

    // backend problems

    pub(crate) fn no_backend(vhost: VirtualHost, rule: Option<usize>, backend: BackendId) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoBackend {
                vhost,
                rule,
                backend,
            }),
        }
    }

    pub(crate) fn no_reachable_endpoints(vhost: VirtualHost, backend: BackendId) -> Self {
        Self {
            inner: Box::new(ErrorImpl::NoReachableEndpoints { vhost, backend }),
        }
    }

    // methods
}

#[derive(Debug, thiserror::Error)]
enum ErrorImpl {
    #[error("invalid url: {0}")]
    InvalidUrl(Cow<'static, str>),

    #[error("invalid route configuration")]
    InvalidRoute {
        message: &'static str,
        vhost: VirtualHost,
        rule: usize,
    },

    #[error(
        "no routing info is available for any of the following vhosts: [{}]",
        format_vhosts(.routes)
    )]
    NoRouteMatched { routes: Vec<VirtualHost> },

    #[error("using route '{route}': no routing rules matched the request")]
    NoRuleMatched { route: VirtualHost },

    #[error("{vhost}: backend not found: {backend}")]
    NoBackend {
        vhost: VirtualHost,
        rule: Option<usize>,
        backend: BackendId,
    },

    #[error("{vhost}: no reachable endpoints")]
    NoReachableEndpoints {
        vhost: VirtualHost,
        backend: BackendId,
    },
}

fn format_vhosts(vhosts: &[VirtualHost]) -> String {
    vhosts
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
