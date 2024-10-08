mod backend;
mod http;
mod shared;

use xds_api::pb::envoy::config::core::v3 as xds_core;

pub(crate) fn ads_config_source() -> xds_core::ConfigSource {
    xds_core::ConfigSource {
        config_source_specifier: Some(xds_core::config_source::ConfigSourceSpecifier::Ads(
            xds_core::AggregatedConfigSource {},
        )),
        resource_api_version: xds_core::ApiVersion::V3 as i32,
        ..Default::default()
    }
}
