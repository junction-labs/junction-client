use std::str::FromStr;

use crate::{error::Error, shared::Regex, Duration};
use std::time::Duration as StdDuration;

pub(crate) fn parse_xds_regex(
    p: &xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher,
) -> Result<Regex, Error> {
    Regex::from_str(&p.regex).map_err(|e| Error::new(format!("invalid regex: {e}")))
}

pub(crate) fn regex_matcher(
    regex: &Regex,
) -> xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher {
    xds_api::pb::envoy::r#type::matcher::v3::RegexMatcher {
        regex: regex.to_string(),
        engine_type: None,
    }
}

impl TryFrom<xds_api::pb::google::protobuf::Duration> for Duration {
    type Error = Error;

    fn try_from(
        proto_duration: xds_api::pb::google::protobuf::Duration,
    ) -> Result<Self, Self::Error> {
        let duration: StdDuration = proto_duration
            .try_into()
            .map_err(|e| Error::new(format!("invalid duration: {e}")))?;

        Ok(duration.into())
    }
}

impl TryFrom<Duration> for xds_api::pb::google::protobuf::Duration {
    type Error = std::num::TryFromIntError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        let seconds = value.as_secs().try_into()?;
        let nanos = value.subsec_nanos().try_into()?;
        Ok(xds_api::pb::google::protobuf::Duration { seconds, nanos })
    }
}
