use std::borrow::Cow;

use junction_api::{BackendId, VirtualHost};

/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(Cow<'static, str>),

    #[error("invalid route configuration")]
    InvalidRoutes {
        message: &'static str,
        vhost: VirtualHost,
        rule: usize,
    },

    #[error("invalid backend configuration")]
    InvalidBackends {
        message: &'static str,
        backend: BackendId,
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
