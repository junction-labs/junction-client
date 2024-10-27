use std::borrow::Cow;

use junction_api::{BackendTarget, RouteTarget};

/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(Cow<'static, str>),

    #[error("invalid route configuration")]
    InvalidRoutes {
        message: &'static str,
        target: RouteTarget,
        rule: usize,
    },

    #[error("invalid backend configuration")]
    InvalidBackends {
        message: &'static str,
        target: BackendTarget,
    },

    #[error(
        "no routing info is available for any of the following targets: [{}]",
        format_targets(.routes)
    )]
    NoRouteMatched { routes: Vec<RouteTarget> },

    #[error("using route '{route}': no routing rules matched the request")]
    NoRuleMatched { route: RouteTarget },

    #[error("{route}: backend not found: {backend}")]
    NoBackend {
        route: RouteTarget,
        rule: usize,
        backend: BackendTarget,
    },

    #[error("{backend}: no reachable endpoints")]
    NoReachableEndpoints {
        route: RouteTarget,
        backend: BackendTarget,
    },
}

fn format_targets(targets: &[RouteTarget]) -> String {
    targets
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Error {
    pub(crate) fn invalid_url(message: String) -> Self {
        Self::InvalidUrl(Cow::Owned(message))
    }

    pub(crate) fn is_temporary(&self) -> bool {
        matches!(
            self,
            Error::NoRouteMatched { .. }
                | Error::NoBackend { .. }
                | Error::NoRuleMatched { .. }
                | Error::NoReachableEndpoints { .. }
        )
    }
}
