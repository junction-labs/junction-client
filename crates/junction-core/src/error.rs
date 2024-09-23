/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(&'static str),

    #[error("ADS connection lost, client has crashed")]
    AdsConnectionLost,

    #[error("no configuration for hostname: {reason}")]
    NotReady { reason: &'static str },

    #[error("no route matched this request")]
    NoRouteMatched,

    #[error("no reachable endpoints")]
    NoReachableEndpoints,
}

impl Error {
    pub(crate) fn is_temporary(&self) -> bool {
        matches!(
            self,
            Error::NotReady { .. } | Error::NoRouteMatched | Error::NoReachableEndpoints
        )
    }
}
