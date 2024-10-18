use std::str::FromStr;

use crate::{error::Error, shared::Regex};

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
