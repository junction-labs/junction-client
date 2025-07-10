use junction_api::{backend::Backend, http::Route};
use junction_core::{HttpResult, ResourceVersion};
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
use std::{net::IpAddr, str::FromStr, time::SystemTime};
use tracing_subscriber::EnvFilter;
use xds_api::pb::google::protobuf;

mod runtime;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD: &str = env!("BUILD_SHA");

#[pymodule]
fn junction(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("_version", VERSION)?;
    m.add("_build", BUILD)?;
    m.add_class::<Junction>()?;
    m.add_class::<Endpoint>()?;
    m.add_class::<Retries>()?;
    m.add_class::<SearchConfig>()?;
    m.add_function(wrap_pyfunction!(default_client, m)?)?;
    m.add_function(wrap_pyfunction!(check_route, m)?)?;
    m.add_function(wrap_pyfunction!(dump_kube_route, m)?)?;
    m.add_function(wrap_pyfunction!(dump_kube_backend, m)?)?;
    m.add_function(wrap_pyfunction!(enable_tracing, m)?)?;

    Ok(())
}

/// Enable Rust's tracing output to stdout, using the `RUST_LOG` environment
/// variable for control over what's logged.
///
/// If `json` is True, traces are output as JSON instead of human-readable text.
#[pyfunction]
#[pyo3(signature = (*, json=false))]
fn enable_tracing(json: bool) -> bool {
    let builder = tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env());

    if json {
        builder.json().try_init().is_ok()
    } else {
        builder.try_init().is_ok()
    }
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
    inner: junction_core::Endpoint,
}

#[pymethods]
impl Endpoint {
    fn __repr__(&self) -> String {
        format!(
            "Endpoint({addr}, {uri})",
            addr = self.inner.addr(),
            uri = self.inner.url(),
        )
    }

    #[getter]
    fn scheme(&self) -> &str {
        self.inner.url().scheme()
    }

    #[getter]
    fn addr(&self) -> IpAddr {
        self.inner.addr().ip()
    }

    #[getter]
    fn port(&self) -> u16 {
        self.inner.addr().port()
    }

    #[getter]
    fn hostname(&self) -> &str {
        self.inner.url().hostname()
    }

    #[getter]
    fn retry_policy(&self) -> Option<Retries> {
        self.inner.retry().clone().map(|r| r.into())
    }

    #[getter]
    fn timeout_policy(&self) -> Option<Timeouts> {
        self.inner.timeouts().clone().map(|t| t.into())
    }

    #[getter]
    fn trace<'py>(&self) -> Trace {
        let trace = self.inner.trace().clone();
        trace.into()
    }
}

impl From<junction_core::Endpoint> for Endpoint {
    fn from(inner: junction_core::Endpoint) -> Self {
        Self { inner }
    }
}

/// A policy that describes how a client should retry requests.
#[derive(Clone, Debug)]
#[pyclass]
pub struct Retries {
    /// The HTTP error codes that retries should be applied to.
    #[pyo3(get)]
    codes: Vec<u16>,

    /// The total number of attempts to make when retrying this request. If
    /// unset, the client should only ever make a single request.
    #[pyo3(get)]
    attempts: u32,

    /// The initial amount of time to back off between requests during a series
    /// of retries. Backoff may scale up to `max_backoff` between requests at
    /// the client's discretion
    #[pyo3(get)]
    backoff: f64,
}

#[pymethods]
impl Retries {
    #[new]
    fn new(codes: Option<Vec<u16>>, attempts: Option<u32>, backoff: Option<f64>) -> Self {
        Self {
            codes: codes.unwrap_or_default(),
            attempts: attempts.unwrap_or_default(),
            backoff: backoff.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Retries({codes:#?}, {attempts}, {backoff})",
            codes = self.codes,
            attempts = self.attempts,
            backoff = self.backoff,
        )
    }
}

impl From<junction_core::Retries> for Retries {
    fn from(value: junction_core::Retries) -> Self {
        Self {
            codes: value.codes,
            attempts: value.attempts.unwrap_or(1),
            backoff: value.backoff.map(|x| x.as_secs_f64()).unwrap_or(0.0),
        }
    }
}

/// A policy that describes how a client should do timeouts.
#[derive(Clone, Debug)]
#[pyclass]
pub struct Timeouts {
    #[pyo3(get)]
    attempt: f64,

    #[pyo3(get)]
    total: f64,
}

#[pymethods]
impl Timeouts {
    fn __repr__(&self) -> String {
        format!(
            "Timeouts({attempt}, {total})",
            attempt = self.attempt,
            total = self.total,
        )
    }
}

impl From<junction_core::Timeouts> for Timeouts {
    fn from(value: junction_core::Timeouts) -> Self {
        Self {
            attempt: value.attempt.map(|x| x.as_secs_f64()).unwrap_or(0.0),
            total: value.total.map(|x| x.as_secs_f64()).unwrap_or(0.0),
        }
    }
}

/// Configuration for searching for a route with check_route.
#[derive(Clone, Debug)]
#[pyclass]
pub struct SearchConfig {
    #[pyo3(get)]
    ndots: u8,

    #[pyo3(get)]
    search: Vec<String>,
}

#[pymethods]
impl SearchConfig {
    #[new]
    fn new(ndots: Option<u8>, search: Option<Vec<String>>) -> Self {
        Self {
            ndots: ndots.unwrap_or_default(),
            search: search.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "SearchConfig({ndots}, {search:#?})",
            ndots = self.ndots,
            search = self.search,
        )
    }
}

impl From<junction_core::SearchConfig> for SearchConfig {
    fn from(value: junction_core::SearchConfig) -> Self {
        Self {
            ndots: value.ndots,
            search: value.search.into_iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[pyclass]
pub struct Trace {
    #[pyo3(get)]
    start: f64,

    #[pyo3(get)]
    events: Vec<TraceEvent>,
}

#[pymethods]
impl Trace {
    fn __repr__(&self) -> String {
        format!("{self:?}")
    }

    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let any = pythonize::pythonize(py, self)?;
        Ok(any)
    }
}

impl From<junction_core::trace::Trace> for Trace {
    fn from(value: junction_core::trace::Trace) -> Self {
        let trace = value.clone();
        Self {
            start: epoch_seconds(trace.start()),
            events: trace.events().cloned().map(TraceEvent::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[pyclass]
pub struct TraceEvent {
    #[pyo3(get)]
    kind: String,

    #[pyo3(get)]
    phase: String,

    #[pyo3(get)]
    time: f64,

    #[pyo3(get)]
    fields: Vec<(String, String)>,
}

#[pymethods]
impl TraceEvent {
    fn __repr__(&self) -> String {
        format!("{self:?}")
    }
}

impl From<junction_core::trace::Event> for TraceEvent {
    fn from(value: junction_core::trace::Event) -> Self {
        Self {
            kind: value.kind().to_string(),
            phase: value.phase().to_string(),
            time: epoch_seconds(value.time()),
            fields: value
                .fields()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

#[inline]
fn epoch_seconds(t: SystemTime) -> f64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .expect("timestamp before unix epoch")
        .as_secs_f64()
}

/// Check route resolution.
///
/// Resolve a request against a routing table. Returns the full route that was
/// selected based on the URL, the index of the routing rule that matched, and
/// the target that the rule resolved to.
///
/// This function is stateless, and doesn't require connecting to a control
/// plane. Use it to unit test your routing rules.
#[pyfunction]
#[pyo3(signature = (routes, url, *, method=None, headers=None, search_config=None))]
fn check_route(
    routes: Bound<'_, PyAny>,
    url: &str,
    method: Option<&str>,
    headers: Option<&Bound<PyMapping>>,
    search_config: Option<Bound<'_, PyAny>>,
) -> PyResult<String> {
    let url: junction_core::Url = url
        .parse()
        .map_err(|e| PyValueError::new_err(format!("{e}")))?;
    let method = method_from_py(method)?;
    let headers = headers_from_py(headers)?;
    let search_config = search_config
        .map(|search_config| pythonize::depythonize_bound(search_config))
        .transpose()?;

    let routes: Vec<Route> = pythonize::depythonize_bound(routes)?;
    let cluster_name =
        junction_core::check_route(routes, search_config.as_ref(), &method, &url, &headers)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to resolve: {e}")))?;

    Ok(cluster_name)
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
    Ok(serde_yaml::to_string(&kube_route)
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

    Ok(serde_yaml::to_string(&patch)
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
    runtime::block_and_check_signals(async {
        junction_core::Client::builder(node_name, cluster_name)
            .build(ads_address)
            .await
            .map_err(|e| match e.source() {
                Some(cause) => format!("ads connection failed: {e}: {cause}"),
                None => format!("ads connection failed: {e}"),
            })
    })
}

/// Return a default Junction client. This client will be used by library
/// integrations if they're not explicitly constructed with a client.
///
/// This client can be configured with an ADS server address and node info by
/// setting the JUNCTION_ADS_SERVER, JUNCTION_NODE, and JUNCTION_CLUSTER
/// environment variables.
#[pyfunction]
fn default_client() -> PyResult<Junction> {
    let core = DEFAULT_CLIENT
        .as_ref()
        .map_err(|e| PyRuntimeError::new_err(e))?
        .clone();

    Ok(Junction { core })
}

#[pymethods]
impl Junction {
    /// Create a new Junction client. The client can be shared and is safe to
    /// use from multiple threads or tasks.
    #[new]
    #[pyo3(signature = (
        *,
        ads_server=None,
        node=None,
        cluster=None,
    ))]
    fn new(
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

        let core = new_client(ads, node, cluster).map_err(PyRuntimeError::new_err)?;
        Ok(Junction { core })
    }

    /// Resolve an endpoint based on an HTTP method, url, and headers.
    ///
    /// Returns the list of endpoints that traffic should be directed to, taking
    /// in to account load balancing and any prior requests. A request should be
    /// sent to all endpoints, and it's up to the caller to decide how to
    /// combine multiple responses.
    #[pyo3(signature = (url, *, method=None, headers=None))]
    fn resolve_http(
        &self,
        url: &str,
        method: Option<&str>,
        headers: Option<&Bound<PyMapping>>,
    ) -> PyResult<Endpoint> {
        let url =
            junction_core::Url::from_str(url).map_err(|e| PyValueError::new_err(format!("{e}")))?;
        let method = method_from_py(method)?;
        let headers = headers_from_py(headers)?;

        let endpoint =
            runtime::block_and_check_signals(self.core.resolve_http(&method, &url, &headers))?;

        Ok(endpoint.into())
    }

    #[pyo3(signature = (*, endpoint, status_code=None, error=None))]
    fn report_status(
        &self,
        endpoint: Endpoint,
        status_code: Option<u16>,
        error: Option<Bound<PyAny>>,
    ) -> PyResult<Endpoint> {
        let result = match (status_code, error) {
            (Some(code), _) => HttpResult::from_u16(code)
                .map_err(|_| PyValueError::new_err("invalid status code"))?,
            (None, Some(_)) => HttpResult::StatusFailed,
            (None, None) => {
                return Err(PyValueError::new_err(
                    "either status_code or error is required",
                ))
            }
        };

        let endpoint =
            runtime::block_and_check_signals(self.core.report_status(endpoint.inner, result))?;

        Ok(endpoint.into())
    }

    /// Spawn a new CSDS server on the given port. Spawning the server will not
    /// block the current thread.
    fn run_csds_server(&self, port: u16) -> PyResult<()> {
        let run_server = self.core.clone().csds_server(port);
        // FIXME: figure out how to report an error better than this. just
        // printing the exception is good buuuuuuut.
        runtime::spawn(async move {
            if let Err(e) = run_server.await {
                let py_err = PyRuntimeError::new_err(format!("csds server exited: {e}"));
                Python::with_gil(|py| py_err.print(py));
            }
        });
        Ok(())
    }

    /// Dump the client's current xDS config as a pbjson dict.
    ///
    /// The xDS config will contain the latest values for all resources and any
    /// errors encountered while trying to fetch updated versions.
    #[pyo3(signature = ())]
    fn dump_xds(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let mut values = vec![];

        for config in self.core.dump_xds() {
            let config: XdsConfig = config.into();
            let as_py = pythonize::pythonize(py, &config)?;
            values.push(as_py);
        }

        Ok(values)
    }

    /// Dump the client's current xDS errors as a pbjson dict.
    ///
    /// This is the same as dumping config with dump_xds and filtering to only
    /// xds with a `last_error` message set.
    fn dump_xds_errors(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let mut values = vec![];

        for config in self.core.dump_xds_errors() {
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

    type_url: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<ResourceVersion>,

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
            type_url: value.type_url,
            version: value.version,
            xds: value.xds,
            error_info,
        }
    }
}

fn method_from_py(method: Option<&str>) -> PyResult<http::Method> {
    match method {
        Some(method) => http::Method::from_str(method)
            .map_err(|_| PyValueError::new_err(format!("invalid HTTP method: '{method}'"))),
        None => Ok(http::Method::GET),
    }
}

fn headers_from_py(header_dict: Option<&Bound<PyMapping>>) -> PyResult<http::HeaderMap> {
    macro_rules! str_value_into {
        ($value:expr) => {
            $value.str()?.to_string_lossy().as_bytes().try_into()
        };
    }

    let Some(header_dict) = header_dict else {
        return Ok(http::HeaderMap::new());
    };

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
