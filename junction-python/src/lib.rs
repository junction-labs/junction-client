use std::{env, net::IpAddr, str::FromStr};

use http::Uri;
use junction_core::ResourceVersion;
use once_cell::sync::Lazy;
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    pyclass, pyfunction, pymethods, pymodule,
    types::{
        PyAnyMethods, PyDict, PyMapping, PyMappingMethods, PyModule, PySequenceMethods,
        PyStringMethods,
    },
    wrap_pyfunction, Bound, Py, PyAny, PyResult, Python,
};
use serde::Serialize;
use xds_api::pb::google::protobuf;

#[pymodule]
fn junction(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<JunctionClient>()?;
    m.add_function(wrap_pyfunction!(run_csds, m)?)?;

    Ok(())
}

#[derive(Debug)]
#[pyclass]
pub struct Endpoint {
    #[pyo3(get)]
    addr: EndpointAddress,

    #[pyo3(get)]
    scheme: String,

    #[pyo3(get)]
    host: String,

    #[pyo3(get)]
    request_uri: String,
}

#[derive(Debug, Clone)]
#[pyclass]
enum EndpointAddress {
    SocketAddr { addr: IpAddr, port: u32 },
    DnsName { name: String, port: u32 },
}

#[pymethods]
impl EndpointAddress {
    fn __repr__(&self) -> String {
        self.to_string()
    }

    fn __str__(&self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for EndpointAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointAddress::SocketAddr { addr, port } => write!(f, "{addr}:{port}"),
            EndpointAddress::DnsName { name, port } => write!(f, "{name}:{port}"),
        }
    }
}

impl From<junction_core::EndpointAddress> for EndpointAddress {
    fn from(addr: junction_core::EndpointAddress) -> Self {
        match addr {
            junction_core::EndpointAddress::SocketAddr(addr) => Self::SocketAddr {
                addr: addr.ip(),
                port: addr.port() as u32,
            },
            junction_core::EndpointAddress::DnsName(name, port) => Self::DnsName { name, port },
        }
    }
}

impl From<junction_core::Endpoint> for Endpoint {
    fn from(ep: junction_core::Endpoint) -> Self {
        let scheme = ep.url.scheme().to_string();
        let host = ep.url.hostname().to_string();
        let request_uri = ep.url.to_string();
        let addr = ep.address.into();

        Self {
            scheme,
            host,
            request_uri,
            addr,
        }
    }
}

#[pyclass]
#[derive(Clone)]
pub struct JunctionClient {
    core: junction_core::Client,
}

static RUNTIME: once_cell::sync::Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("junction")
        .build()
        .expect("failed to initialize a junction runtime");

    rt
});

static DEFAULT_CLIENT: Lazy<PyResult<junction_core::Client>> = Lazy::new(|| {
    let ads_address =
        env::var("JUNCTION_ADS_SERVER").unwrap_or_else(|_| "grpc://127.0.0.1:8080".to_string());
    let node_name =
        env::var("JUNCTION_NODE_NAME").unwrap_or_else(|_| "junction-python".to_string());
    let cluster_name = env::var("JUNCTION_CLUSTER").unwrap_or_default();

    let rt = &RUNTIME;
    rt.block_on(junction_core::Client::build(
        ads_address,
        node_name,
        cluster_name,
        vec![],
    ))
    .map_err(|e| {
        let error_message = match e.source() {
            Some(cause) => format!("ads connection failed: {e}: {cause}"),
            None => format!("ads connection failed: {e}"),
        };
        PyRuntimeError::new_err(error_message)
    })
});

#[pyfunction]
fn run_csds(port: u16) -> PyResult<()> {
    match DEFAULT_CLIENT.as_ref() {
        Ok(client) => {
            RUNTIME.spawn(client.config_server(port));
            Ok(())
        }
        Err(e) => Err(PyRuntimeError::new_err(e)),
    }
}

#[pymethods]
impl JunctionClient {
    #[staticmethod]
    #[pyo3(signature = (**kwargs))]
    fn new_client(kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let routes = match kwargs {
            Some(route_dict) => {
                let default_routes = route_dict.get_item("default_routes")?;
                pythonize::depythonize_bound(default_routes)?
            }
            None => Vec::new(),
        };

        match DEFAULT_CLIENT.as_ref() {
            Ok(client) => {
                let core = client.clone().with_default_routes(routes);
                Ok(JunctionClient { core })
            }
            Err(e) => Err(PyRuntimeError::new_err(e)),
        }
    }

    fn resolve_endpoints(
        &mut self,
        method: &str,
        url: &str,
        headers: &Bound<PyMapping>,
    ) -> PyResult<Vec<Endpoint>> {
        let url: Uri = url
            .parse()
            .map_err(|e| PyValueError::new_err(format!("invalid url: {e}")))?;

        if url.scheme().is_none() {
            return Err(PyValueError::new_err("url must have a valid scheme"));
        }
        if url.host().is_none() {
            return Err(PyValueError::new_err("url must have a host"));
        }

        let method = http::Method::from_str(method)
            .map_err(|_| PyValueError::new_err(format!("invalid HTTP method: {method}")))?;

        let headers = headers_from_py(headers)?;

        let endpoints = self
            .core
            .resolve_endpoints(&method, url, &headers)
            .map(|endpoints| endpoints.into_iter().map(|e| e.into()).collect())
            .map_err(|e| PyRuntimeError::new_err(format!("failed to resolve: {e}")))?;

        Ok(endpoints)
    }

    fn dump_xds(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let mut values = vec![];

        for config in self.core.dump() {
            let config: XdsConfig = config.into();
            let as_py = pythonize::pythonize(py, &config)?;
            values.push(as_py);
        }

        Ok(values)
    }
}

#[derive(Debug, Serialize)]
struct XdsConfig {
    name: String,

    version: ResourceVersion,

    #[serde(skip_serializing_if = "Option::is_none")]
    xds: Option<protobuf::Any>,

    #[serde(skip_serializing_if = "Option::is_none")]
    error_info: Option<XdsErrorInfo>,
}

#[derive(Debug, Serialize)]
struct XdsErrorInfo {
    version: ResourceVersion,
    message: String,
}

impl From<junction_core::XdsConfig> for XdsConfig {
    fn from(value: junction_core::XdsConfig) -> Self {
        let error_info = value.last_error.map(|(v, e)| XdsErrorInfo {
            version: v,
            message: e,
        });

        Self {
            name: value.name,
            version: value.version,
            xds: value.xds,
            error_info,
        }
    }
}

fn headers_from_py(header_dict: &Bound<PyMapping>) -> PyResult<http::HeaderMap> {
    macro_rules! str_value_into {
        ($value:expr) => {
            $value.str()?.to_string_lossy().as_bytes().try_into()
        };
    }
    let items = header_dict.items()?;

    let mut headers = http::HeaderMap::with_capacity(items.len()?);
    for item in items.iter()? {
        let item = item?;
        let key = item.get_item(0)?;
        let value = item.get_item(1)?;

        let header_name: http::HeaderName = str_value_into!(key)
            .map_err(|e| PyValueError::new_err(format!("invalid http header name: {e}")))?;
        let header_value = str_value_into!(value)
            .map_err(|e| PyValueError::new_err(format!("invalid http header value: {e}")))?;

        headers.insert(header_name, header_value);
    }

    Ok(headers)
}
