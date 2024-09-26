/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(&'static str),

    #[error("ADS connection lost, client has crashed")]
    AdsConnectionLost,

    #[error("no route matched the hostname and port")]
    NoRouteMatched,

    #[error("no rule within the matching route matched the request parameters")]
    NoRuleMatched,

    #[error("the matched backend does not have any endpoints")]
    NoEndpoints,

    #[error("the matched backend does not have any reachable endpoints")]
    NoReachableEndpoints,
}

impl Error {
    pub(crate) fn is_temporary(&self) -> bool {
        matches!(
            self,
            Error::NoRouteMatched | Error::NoRuleMatched | Error::NoEndpoints
        )
    }
}
