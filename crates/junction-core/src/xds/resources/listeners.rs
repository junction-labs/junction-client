use super::{ErrorCtx, Resource, ResourceError, ResourceName, ResourceType, RouteConfiguration};

use xds_api::pb::envoy::{
    config::listener::v3 as xds_listener,
    extensions::filters::network::http_connection_manager::v3 as xds_http,
};

#[derive(Debug, Clone)]
pub(crate) struct ApiListener {
    pub route_config: RouteConfig,
}

#[derive(Debug, Clone)]
pub(crate) enum RouteConfig {
    Rds(ResourceName),
    Inline(RouteConfiguration),
}

impl Resource for ApiListener {
    type Xds = xds_listener::Listener;

    fn from_xds(xds: &Self::Xds) -> Result<Self, ResourceError> {
        use xds_http::http_connection_manager::RouteSpecifier;

        let conn_manager = http_connection_manager(&xds).with_field("api_listener")?;

        let route_config = match &conn_manager.route_specifier {
            Some(RouteSpecifier::Rds(rds)) => {
                let name = ResourceName::from(rds.route_config_name.clone());
                RouteConfig::Rds(name)
            }
            Some(RouteSpecifier::RouteConfig(route_config)) => {
                let inline = RouteConfiguration::from_xds(route_config)
                    .with_fields("api_listener", "route_specifier")?;
                RouteConfig::Inline(inline)
            }
            _ => return Err(ResourceError::invalid("no routes configured")),
        };

        Ok(Self { route_config })
    }

    fn references(&self) -> Vec<(super::ResourceType, ResourceName)> {
        match &self.route_config {
            RouteConfig::Rds(name) => vec![(ResourceType::RouteConfiguration, name.clone())],
            RouteConfig::Inline(route_config) => route_config.references(),
        }
    }
}

fn http_connection_manager(
    listener: &xds_listener::Listener,
) -> Result<xds_http::HttpConnectionManager, ResourceError> {
    let api_listener = listener
        .api_listener
        .as_ref()
        .and_then(|l| l.api_listener.as_ref())
        .ok_or_else(|| ResourceError::invalid("missing api_listener"))?;

    api_listener
        .to_msg()
        .map_err(|e| ResourceError::invalid_with(format!("invalid api_listener: {e}")))
}
