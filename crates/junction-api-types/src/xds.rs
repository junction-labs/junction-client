/// An error that occurred while handling an XDS message.
///
/// These errors are built while decoding, validating, and transforming XDS and
/// may be exposed to clients via debug outputs or ADS servers via XDS NACKs.
#[derive(Clone, Debug, thiserror::Error)]
// TODO: unsupported xds as a variant?
// TODO: might be nice to include some constructors that do type URL things with type magick
pub enum Error {
    #[error("invalid xds: {resource_name} '{resource_type}': {message}")]
    InvalidXds {
        resource_type: &'static str,
        resource_name: String,
        message: String,
    },
}
