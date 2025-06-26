use std::borrow::Cow;

use crate::Trace;

/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

/// An error when using the Junction client.
#[derive(Debug, thiserror::Error)]
#[error("{inner}")]
pub struct Error {
    // a trace of what's happened so far
    trace: Option<Trace>,

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
        matches!(*self.inner, ErrorImpl::TimedOut { .. })
    }
}

impl Error {
    pub(crate) fn into_invalid_url(message: String) -> Self {
        let inner = ErrorImpl::InvalidUrl(Cow::Owned(message));
        Self {
            trace: None,
            inner: Box::new(inner),
        }
    }

    pub(crate) fn invalid_url(message: &'static str) -> Self {
        let inner = ErrorImpl::InvalidUrl(Cow::Borrowed(message));
        Self {
            trace: None,
            inner: Box::new(inner),
        }
    }

    pub(crate) fn timed_out(message: &'static str, trace: Trace) -> Self {
        let inner = ErrorImpl::TimedOut(Cow::from(message));
        Self {
            trace: Some(trace),
            inner: Box::new(inner),
        }
    }

    pub(crate) fn no_route_matched(authority: String, trace: Trace) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::NoRouteMatched { authority }),
        }
    }

    pub(crate) fn not_found(resource_type: String, resource_name: String, trace: Trace) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::NotFound {
                resource_type,
                resource_name,
            }),
        }
    }

    pub(crate) fn unavailable(trace: Trace, msg: &'static str) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::Unavailable(Cow::Borrowed(msg))),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ErrorImpl {
    #[error("invalid url: {0}")]
    InvalidUrl(Cow<'static, str>),

    #[error("timed out {0}")]
    TimedOut(Cow<'static, str>),

    #[error("no route matched: '{authority}'")]
    NoRouteMatched { authority: String },

    #[error("xds {resource_type} not found: {resource_name}")]
    NotFound {
        resource_type: String,
        resource_name: String,
    },

    #[error("unavailable")]
    Unavailable(Cow<'static, str>),
}
