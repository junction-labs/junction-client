use std::str::FromStr;

use http::uri::{Authority, PathAndQuery, Scheme};

use crate::Error;

/// An [http::Uri] that has an `http` or `https` scheme and a non-empty
/// `authority`.
///
/// The `authority` section of a `Url` must contains a hostname and may contain
/// a port, but must not contain a username or password.
///
/// ```ascii
/// https://example.com:123/path/data?key=value&key2=value2#fragid1
/// ─┬───  ──────────┬──── ─────┬──── ───────┬─────────────────────
///  │               │          │            │
///  └─scheme        │     path─┘            │
///                  │                       │
///        authority─┘                 query─┘
/// ```
///
/// There are no extra restrictions on the path or query components of a valid
/// `Url`.
#[derive(Clone, Debug)]
pub struct Url {
    scheme: Scheme,
    authority: Authority,
    path_and_query: PathAndQuery,
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{scheme}://{authority}{path}",
            scheme = self.scheme,
            authority = self.authority,
            path = self.path(),
        )?;

        if let Some(query) = self.query() {
            write!(f, "?{query}")?;
        }

        Ok(())
    }
}

// FIXME: use some of the Bytes machinery to avoid copying input uris
impl Url {
    pub fn new(uri: http::Uri) -> crate::Result<Self> {
        let uri = uri.into_parts();

        let Some(authority) = uri.authority else {
            return Err(Error::invalid_url("missing hostname"));
        };
        if !authority.as_str().starts_with(authority.host()) {
            return Err(Error::invalid_url(
                "url must not contain a username or password",
            ));
        }

        let scheme = match uri.scheme.as_ref().map(|s| s.as_str()) {
            Some("http") | Some("https") => uri.scheme.unwrap(),
            Some(_) => return Err(Error::invalid_url("unknown scheme")),
            _ => return Err(Error::invalid_url("missing scheme")),
        };
        let path_and_query = uri
            .path_and_query
            .unwrap_or_else(|| PathAndQuery::from_static("/"));

        Ok(Self {
            scheme,
            authority,
            path_and_query,
        })
    }
}

impl FromStr for Url {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uri = http::Uri::from_str(s).map_err(|e| Error::into_invalid_url(e.to_string()))?;

        Self::new(uri)
    }
}

impl Url {
    pub fn scheme(&self) -> &str {
        self.scheme.as_str()
    }

    pub fn authority(&self) -> &str {
        self.authority.as_str()
    }

    pub fn hostname(&self) -> &str {
        self.authority.host()
    }

    pub fn port(&self) -> Option<u16> {
        self.authority.port_u16()
    }

    pub fn default_port(&self) -> u16 {
        self.authority
            .port_u16()
            .unwrap_or_else(|| match self.scheme.as_ref() {
                "https" => 443,
                _ => 80,
            })
    }

    pub fn path(&self) -> &str {
        self.path_and_query.path()
    }

    pub fn query(&self) -> Option<&str> {
        self.path_and_query.query()
    }

    pub fn request_uri(&self) -> &str {
        self.path_and_query.as_str()
    }
}
