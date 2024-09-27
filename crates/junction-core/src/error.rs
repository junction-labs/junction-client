use junction_api_types::shared::Attachment;

/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(&'static str),

    #[error("invalid route configuration")]
    InvalidRoutes {
        message: &'static str,
        attachment: Attachment,
        rule: usize,
    },

    #[error("invalid backend configuration")]
    InvalidBackends {
        message: &'static str,
        attachment: Attachment,
    },

    #[error(
        "no routing info is available for any of the following targets: [{}]",
        format_attachments(.routes)
    )]
    NoRouteMatched { routes: Vec<Attachment> },

    #[error("using route '{route}': no routing rules matched the request")]
    NoRuleMatched { route: Attachment },

    #[error("{route}: backend not found: {backend}")]
    NoBackend {
        route: Attachment,
        rule: usize,
        backend: Attachment,
    },

    #[error("{backend}: no endpoints are healthy")]
    NoReachableEndpoints {
        route: Attachment,
        backend: Attachment,
    },
}

fn format_attachments(attachments: &[Attachment]) -> String {
    attachments
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
