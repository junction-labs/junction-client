use junction_api::{
    backend::{Backend, BackendId},
    http::Route,
};
use junction_core::{ResolveMode, ResourceVersion};
use once_cell::sync::Lazy;
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    pyclass, pyfunction, pymethods, pymodule,
    types::{
        PyAnyMethods, PyMapping, PyMappingMethods, PyModule, PySequenceMethods, PyStringMethods,
    },
    wrap_pyfunction, Bound, Py, PyAny, PyResult, Python,
};
use serde::Serialize;
use std::{net::IpAddr, str::FromStr};
use xds_api::pb::google::protobuf;

#[pymodule]
fn junction(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Junction>()?;
    m.add_function(wrap_pyfunction!(default_client, m)?)?;
    m.add_function(wrap_pyfunction!(check_route, m)?)?;
    m.add_function(wrap_pyfunction!(dump_kube_route, m)?)?;
    m.add_function(wrap_pyfunction!(dump_kube_backend, m)?)?;

    Ok(())
}

mod env {
    use super::*;

    pub(super) fn ads_server(arg: Option<String>, message: &'static str) -> PyResult<String> {
        arg.or(std::env::var("JUNCTION_ADS_SERVER").ok())
            .ok_or(PyRuntimeError::new_err(message))
    }

    pub(super) fn node_info(arg: Option<String>) -> String {
        arg.or(std::env::var("JUNCTION_NODE_NAME").ok())
            .unwrap_or_else(|| "junction-python".to_string())
    }

    pub(super) fn cluster_name(arg: Option<String>) -> String {
        arg.or(std::env::var("JUNCTION_CLUSTER").ok())
            .unwrap_or_else(|| "junction-python".to_string())
    }
}

/// An endpoint that an HTTP call can be made to. Includes the address that the
/// request should resolve to along with the original request URI, the scheme to
/// use, and the hostname to use for TLS if appropriate.
#[derive(Clone, Debug)]
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

    #[pyo3(get)]
    retry_policy: Option<RetryPolicy>,

    #[pyo3(get)]
    timeout_policy: Option<TimeoutPolicy>,
}

#[pymethods]
impl Endpoint {
    fn __repr__(&self) -> String {
        format!(
            "Endpoint({addr}, {uri})",
            addr = self.addr,
            uri = self.request_uri
        )
    }
}

/// An endpoint address. An address can either be an IPAddress or a DNS name,
/// but will always include a port.
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

impl From<&junction_core::EndpointAddress> for EndpointAddress {
    fn from(addr: &junction_core::EndpointAddress) -> Self {
        match addr {
            junction_core::EndpointAddress::SocketAddr(addr) => Self::SocketAddr {
                addr: addr.ip(),
                port: addr.port() as u32,
            },
            junction_core::EndpointAddress::DnsName(name, port) => Self::DnsName {
                name: name.clone(),
                port: *port,
            },
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
        let retry_policy = ep.retry.map(|r| r.into());
        let timeout_policy = ep.timeouts.map(|r| r.into());

        Self {
            scheme,
            host,
            request_uri,
            addr,
            retry_policy,
            timeout_policy,
        }
    }
}

//
// FIXME: this works fine if you keep want to retrying the one address, but best
// practices is to vary the addresses on a retry, which requires bigger changes
//
/// A policy that describes how a client should retry requests.
#[derive(Clone, Debug)]
#[pyclass]
pub struct RetryPolicy {
    #[pyo3(get)]
    codes: Vec<u32>,

    #[pyo3(get)]
    attempts: u32,

    #[pyo3(get)]
    backoff: f64,
}

/// A policy that describes how a client should do timeouts.
#[derive(Clone, Debug)]
#[pyclass]
pub struct TimeoutPolicy {
    #[pyo3(get)]
    backend_request: f64,

    #[pyo3(get)]
    request: f64,
}

#[pymethods]
impl RetryPolicy {
    fn __repr__(&self) -> String {
        format!(
            "RetryPolicy({attempts}, {min})",
            //FIXME: add codes
            attempts = self.attempts,
            min = self.backoff,
        )
    }
}

impl From<junction_api::http::RouteRetry> for RetryPolicy {
    fn from(value: junction_api::http::RouteRetry) -> Self {
        Self {
            codes: value.codes,
            attempts: value.attempts.unwrap_or(1),
            backoff: value.backoff.map(|x| x.as_secs_f64()).unwrap_or(0.0),
        }
    }
}

#[pymethods]
impl TimeoutPolicy {
    fn __repr__(&self) -> String {
        format!(
            "TimeoutPolicy({backend_request}, {request})",
            backend_request = self.backend_request,
            request = self.request,
        )
    }
}

impl From<junction_api::http::RouteTimeouts> for TimeoutPolicy {
    fn from(value: junction_api::http::RouteTimeouts) -> Self {
        Self {
            backend_request: value
                .backend_request
                .map(|x| x.as_secs_f64())
                .unwrap_or(0.0),
            request: value.request.map(|x| x.as_secs_f64()).unwrap_or(0.0),
        }
    }
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

/// Check route resolution.
///
/// Resolve a request against a routing table. Returns the full route that was
/// selected based on the URL, the index of the routing rule that matched, and
/// the target that the rule resolved to.
///
/// This function is stateless, and doesn't require connecting to a control
/// plane. Use it to unit test your routing rules.
#[pyfunction]
#[pyo3(signature = (routes, method, url, headers))]
fn check_route(
    py: Python<'_>,
    routes: Bound<'_, PyAny>,
    method: &str,
    url: &str,
    headers: &Bound<PyMapping>,
) -> PyResult<(Py<PyAny>, Option<usize>, Py<PyAny>)> {
    let url: junction_core::Url = url
        .parse()
        .map_err(|e| PyValueError::new_err(format!("{e}")))?;
    let method = method_from_py(method)?;
    let headers = headers_from_py(headers)?;

    let routes: Vec<Route> = pythonize::depythonize_bound(routes)?;
    let resolved = junction_core::check_route(routes, &method, &url, &headers)
        .map_err(|e| PyRuntimeError::new_err(format!("failed to resolve: {e}")))?;

    let route = pythonize::pythonize(py, &resolved.route)?;
    let backend = pythonize::pythonize(py, &resolved.backend)?;

    Ok((route, resolved.rule, backend))
}

/// Dump a Route as Kubernetes YAML.
///
/// The route is dumped as a Gateway API HTTPRoute, ready to be applied and
/// updated. Routes with a Service target have their namespace and name
/// inferred, but routes with other targets need to have namespace and name
/// kwargs set explicitly.
#[pyfunction]
#[pyo3(signature = (*, route, namespace))]
fn dump_kube_route(route: Bound<'_, PyAny>, namespace: String) -> PyResult<String> {
    let route: Route = pythonize::depythonize_bound(route)?;
    let kube_route = route
        .to_gateway_httproute(&namespace)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(serde_yml::to_string(&kube_route)
        .expect("Serialization failed. This is a bug in Junction, not your code."))
}

/// Dump a Backend to Kubernetes YAML.
///
/// Backends are dumped as partial Service objects that can be applied as a
/// patch with `kubectl patch`, or re-parsed and modified to include any missing
/// information about your service.
///
/// Backends with a Service target will include the name and namespace of the
/// target service as part of the patch data. Other targets can't easily
/// infer their name and namespace.
#[pyfunction]
fn dump_kube_backend(backend: Bound<'_, PyAny>) -> PyResult<String> {
    let backend: Backend = pythonize::depythonize_bound(backend)?;
    let patch = backend.to_service_patch();

    Ok(serde_yml::to_string(&patch)
        .expect("Serialization failed. This is a bug in Junction, not your code."))
}

/// A Junction endpoint discovery client.
#[pyclass]
#[derive(Clone)]
pub struct Junction {
    core: junction_core::Client,
}

static DEFAULT_CLIENT: Lazy<PyResult<junction_core::Client>> = Lazy::new(|| {
    let ads = env::ads_server(
        None,
        "JUNCTION_ADS_SERVER isn't set, can't use the default client",
    )?;
    let (node, cluster) = (env::node_info(None), env::cluster_name(None));
    new_client(ads, node, cluster)
});

fn new_client(
    ads_address: String,
    node_name: String,
    cluster_name: String,
) -> PyResult<junction_core::Client> {
    let rt = &RUNTIME;
    rt.block_on(junction_core::Client::build(
        ads_address,
        node_name,
        cluster_name,
    ))
    .map_err(|e| {
        let error_message = match e.source() {
            Some(cause) => format!("ads connection failed: {e}: {cause}"),
            None => format!("ads connection failed: {e}"),
        };
        PyRuntimeError::new_err(error_message)
    })
}

/// Return a default Junction client. This client will be used by library
/// integrations if they're not explicitly constructed with a client.
///
/// This client can be configured with an ADS server address and node info by
/// setting the JUNCTION_ADS_SERVER, JUNCTION_NODE, and JUNCTION_CLUSTER
/// environment variables.
#[pyfunction]
#[pyo3(signature = (*, static_routes=None, static_backends=None))]
fn default_client(
    static_routes: Option<Bound<'_, PyAny>>,
    static_backends: Option<Bound<'_, PyAny>>,
) -> PyResult<Junction> {
    let mut core = match DEFAULT_CLIENT.as_ref() {
        Ok(default_client) => default_client.clone(),
        Err(e) => return Err(PyRuntimeError::new_err(e)),
    };

    let routes = static_routes
        .map(|routes| pythonize::depythonize_bound(routes))
        .transpose()?;

    let backends = static_backends
        .map(|backends| pythonize::depythonize_bound(backends))
        .transpose()?;

    if routes.is_some() || backends.is_some() {
        let routes = routes.unwrap_or_default();
        let backends = backends.unwrap_or_default();
        core = core.with_static_config(routes, backends);
    }

    Ok(Junction { core })
}

#[pymethods]
impl Junction {
    /// Create a new Junction client. The client can be shared and is safe to
    /// use from multiple threads or tasks.
    #[new]
    #[pyo3(signature = (
        *,
        static_routes=None,
        static_backends=None,
        ads_server=None,
        node=None,
        cluster=None,
    ))]
    fn new(
        static_routes: Option<Bound<'_, PyAny>>,
        static_backends: Option<Bound<'_, PyAny>>,
        ads_server: Option<String>,
        node: Option<String>,
        cluster: Option<String>,
    ) -> PyResult<Self> {
        let ads = env::ads_server(
            ads_server,
            "no ads server specified: ads_server wasn't passed and JUNCTION_ADS_SERVER isn't set",
        )?;
        let node = env::node_info(node);
        let cluster = env::cluster_name(cluster);
        let mut core = new_client(ads, node, cluster).map_err(PyRuntimeError::new_err)?;

        let routes = static_routes
            .map(|routes| pythonize::depythonize_bound(routes))
            .transpose()?;
        let backends = static_backends
            .map(|backends| pythonize::depythonize_bound(backends))
            .transpose()?;
        if routes.is_some() || backends.is_some() {
            let routes = routes.unwrap_or_default();
            let backends = backends.unwrap_or_default();
            core = core.with_static_config(routes, backends);
        }

        Ok(Junction { core })
    }

    /// Perform the route resolution half of resolve_http, returning the
    /// matched route, the index of the matching rule, and the backend that
    /// was selected. Use it as a lower level method to debug route resolution,
    /// or to look up Routes without making a full request.
    ///
    /// If `dynamic=False` is passed as a kwarg, the resolution happens without
    /// fetching any new routing data over the network.
    #[pyo3(signature = (method, url, headers, dynamic=true))]
    fn resolve_route(
        &mut self,
        py: Python<'_>,
        method: &str,
        url: &str,
        headers: &Bound<PyMapping>,
        dynamic: bool,
    ) -> PyResult<(Py<PyAny>, Option<usize>, Py<PyAny>)> {
        let method = method_from_py(method)?;
        let url =
            junction_core::Url::from_str(url).map_err(|e| PyValueError::new_err(format!("{e}")))?;
        let headers = headers_from_py(headers)?;

        let request = junction_core::HttpRequest::from_parts(&method, &url, &headers)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;

        let config_mode = match dynamic {
            true => ResolveMode::Dynamic,
            false => ResolveMode::Static,
        };

        let resolved = RUNTIME
            .block_on(self.core.resolve_routes(config_mode, request))
            .map_err(|e| PyRuntimeError::new_err(format!("failed to resolve: {e}")))?;

        let route = pythonize::pythonize(py, &resolved.route)?;
        let backend = pythonize::pythonize(py, &resolved.backend)?;
        Ok((route, resolved.rule, backend))
    }

    /// Return the list of addresses currently in cache for a backend. These
    /// endpoints are a snapshot of what is currently in cache.
    #[pyo3(signature = (backend))]
    fn get_endpoints(&self, backend: Bound<'_, PyAny>) -> PyResult<Vec<EndpointAddress>> {
        let backend: BackendId = pythonize::depythonize_bound(backend)?;
        let endpoint_iter = match self.core.get_endpoints(&backend) {
            Some(iter) => iter,
            None => return Ok(Vec::new()),
        };

        Ok(endpoint_iter.addrs().map(EndpointAddress::from).collect())
    }

    /// Resolve an endpoint based on an HTTP method, url, and headers.
    ///
    /// Returns the list of endpoints that traffic should be directed to, taking
    /// in to account load balancing and any prior requests. A request should be
    /// sent to all endpoints, and it's up to the caller to decide how to
    /// combine multiple responses.
    #[pyo3(signature = (method, url, headers))]
    fn resolve_http(
        &mut self,
        method: &str,
        url: &str,
        headers: &Bound<PyMapping>,
    ) -> PyResult<Vec<Endpoint>> {
        let url =
            junction_core::Url::from_str(url).map_err(|e| PyValueError::new_err(format!("{e}")))?;
        let method = method_from_py(method)?;
        let headers = headers_from_py(headers)?;

        let endpoints = RUNTIME
            .block_on(self.core.resolve_http(&method, &url, &headers))
            .map(|endpoints| endpoints.into_iter().map(|e| e.into()).collect())
            .map_err(|e| PyRuntimeError::new_err(format!("failed to resolve: {e}")))?;

        Ok(endpoints)
    }

    /// Spawn a new CSDS server on the given port. Spawning the server will not
    /// block the current thread.
    fn run_csds_server(&self, port: u16) -> PyResult<()> {
        let run_server = self.core.clone().csds_server(port);
        // FIXME: figure out how to report an error better than this. just
        // printing the exception is good buuuuuuut.
        RUNTIME.spawn(async move {
            if let Err(e) = run_server.await {
                let py_err = PyRuntimeError::new_err(format!("csds server exited: {e}"));
                Python::with_gil(|py| py_err.print(py));
            }
        });
        Ok(())
    }

    /// Dump the client's current route config.
    ///
    /// This is the same merged view of dynamic config and static config that
    /// the client uses to make routing decisions.
    fn dump_routes(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let mut values = vec![];

        for route in self.core.dump_routes() {
            values.push(pythonize::pythonize(py, &route)?);
        }

        Ok(values)
    }

    /// Dump the client's backend config.
    ///
    /// This is the same merged view of dynamic config and static config that
    /// the client uses to make load balancing decisions.
    fn dump_backends(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let mut values = vec![];

        for backend in self.core.dump_backends() {
            values.push(pythonize::pythonize(py, &backend.config)?);
        }

        Ok(values)
    }

    /// Dump the client's current xDS config as a pbjson dict.
    ///
    /// The xDS config will contain the latest values for all resources and any
    /// errors encountered while trying to fetch updated versions.
    fn dump_xds(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let mut values = vec![];

        for config in self.core.dump_xds() {
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

fn method_from_py(method: &str) -> PyResult<http::Method> {
    http::Method::from_str(method)
        .map_err(|_| PyValueError::new_err(format!("invalid HTTP method: '{method}'")))
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
