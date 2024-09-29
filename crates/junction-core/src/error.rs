use std::borrow::Cow;

use junction_api_types::shared::Target;

/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(Cow<'static, str>),

    #[error("invalid route configuration")]
    InvalidRoutes {
        message: &'static str,
        target: Target,
        rule: usize,
    },

    #[error("invalid backend configuration")]
    InvalidBackends {
        message: &'static str,
        target: Target,
    },

    #[error(
        "no routing info is available for any of the following targets: [{}]",
        format_targets(.routes)
    )]
    NoRouteMatched { routes: Vec<Target> },

    #[error("using route '{route}': no routing rules matched the request")]
    NoRuleMatched { route: Target },

    #[error("{route}: backend not found: {backend}")]
    NoBackend {
        route: Target,
        rule: usize,
        backend: Target,
    },

    #[error("{backend}: no endpoints are healthy")]
    NoReachableEndpoints { route: Target, backend: Target },
}

fn format_targets(targets: &[Target]) -> String {
    targets
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Error {
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
