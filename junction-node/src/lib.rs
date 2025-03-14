use std::{future::Future, str::FromStr, sync::Arc, time::Duration};

use neon::{
    handle::Handle,
    object::Object,
    prelude::{Context, FunctionContext, ModuleContext, TaskContext},
    result::{JsResult, NeonResult},
    types::{
        Finalize, JsArray, JsBox, JsNumber, JsObject, JsPromise, JsString, JsUndefined, JsValue,
        Value,
    },
};

// TODO: figure out how to convert objects, the manual approach super sucks and
// won't work for Routes/Backends. figuring out Serde is maybe the right thing,
// and writing our own Ser/De impls doesn't seem horrible? has all the same problems
// that the approach does in Python though.
//
// neon-serde is effectively abandoned or that would be an easy start. it
// depends on neon 0.4.0 https://github.com/GabrielCastro/neon-serde

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD: &str = env!("BUILD_SHA");

#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    let version = cx.string(VERSION);
    cx.export_value("version", version)?;

    let build_version = cx.string(BUILD);
    cx.export_value("build", build_version)?;

    cx.export_function("newRuntime", new_runtime)?;
    cx.export_function("newClient", new_client)?;
    cx.export_function("resolveHttp", resolve_http)?;
    cx.export_function("reportStatus", report_status)?;

    Ok(())
}

macro_rules! arg_value {
    ($ctx:expr, $js_type:ty, $index:expr) => {{
        let handle: Handle<$js_type> = $ctx.argument($index)?;
        handle.value(&mut $ctx)
    }};
}

macro_rules! opt_arg_value {
    ($ctx:expr, $js_type:ty, $index:expr) => {{
        match $ctx.argument_opt($index) {
            Some(arg) if arg.is_a::<JsUndefined, _>(&mut $ctx) => None,
            Some(arg) => {
                let arg: Handle<$js_type> = arg.downcast_or_throw(&mut $ctx)?;
                Some(arg.value(&mut $ctx))
            }
            None => None,
        }
    }};
}

fn arg_from_str<T: FromStr>(cx: &mut FunctionContext, idx: usize) -> NeonResult<T>
where
    T::Err: std::error::Error,
{
    let arg: Handle<JsString> = cx.argument(idx)?;
    match arg.value(cx).parse() {
        Ok(v) => Ok(v),
        Err(e) => cx.throw_error(e.to_string()),
    }
}

fn from_js_str<'a, T: FromStr>(cx: &mut impl Context<'a>, str: &JsString) -> NeonResult<T>
where
    T::Err: std::error::Error,
{
    match str.value(cx).parse() {
        Ok(v) => Ok(v),
        Err(e) => cx.throw_error(e.to_string()),
    }
}

fn new_runtime(mut cx: FunctionContext) -> JsResult<JsBox<Runtime>> {
    match Runtime::new_default() {
        Ok(rt) => Ok(cx.boxed(rt)),
        Err(e) => cx.throw_error(format!("failed to create runtime: {e}")),
    }
}

fn new_client(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let runtime: Handle<JsBox<Runtime>> = cx.argument(0)?;
    let ads_server = arg_value!(cx, JsString, 1);
    let node_id = arg_value!(cx, JsString, 2);
    let cluster_id = arg_value!(cx, JsString, 3);

    let build_client = async move {
        junction_core::Client::build(ads_server, node_id, cluster_id)
            .await
            .map_err(|e| e.to_string())
    };

    Ok(
        runtime.spawn_promise(&mut cx, build_client, |mut cx, res| match res {
            Ok(client) => Ok(cx.boxed(ffi::Client(client))),
            Err(e) => cx.throw_error(e),
        }),
    )
}

fn resolve_http(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let runtime: Handle<JsBox<Runtime>> = cx.argument(0)?;
    let client: Handle<JsBox<ffi::Client>> = cx.argument(1)?;

    let method: http::Method = arg_from_str(&mut cx, 2)?;
    let url: junction_core::Url = arg_from_str(&mut cx, 3)?;

    let headers: Handle<JsArray> = cx.argument(4)?;
    let headers = headers_from_js(&mut cx, headers)?;

    // have to wrap the client.resolve_http call in an async move block so it
    // takes ownership of the client. otherwise it would be nice to make the
    // whole thing just a call to client.resolve_http
    let client = client.cloned();
    let resolve_http = async move { client.resolve_http(&method, &url, &headers).await };

    Ok(
        runtime.spawn_promise(&mut cx, resolve_http, |mut cx, res| match res {
            Ok(endpoint) => js_endpoint(&mut cx, endpoint),
            Err(e) => cx.throw_error(e.to_string()),
        }),
    )
}

fn report_status(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let runtime: Handle<JsBox<Runtime>> = cx.argument(0)?;
    let client: Handle<JsBox<ffi::Client>> = cx.argument(1)?;
    let endpoint: Handle<JsBox<ffi::Endpoint>> = cx.argument(2)?;

    let status_code: Option<f64> = opt_arg_value!(cx, JsNumber, 3);
    let error_msg: Option<String> = opt_arg_value!(cx, JsString, 4);

    let http_result = match (status_code, error_msg) {
        (Some(status_code), _) => {
            // f64 as u16 is a saturating cast that also floors the answer.  any
            // funny business here gets truncated to 0 or u16::MAX instead of
            // panicing. there is no try_into impl as of 1.79
            match junction_core::HttpResult::from_u16(status_code as u16) {
                Ok(result) => result,
                Err(_) => return cx.throw_error("invalid http status code"),
            }
        }
        (None, Some(_)) => junction_core::HttpResult::StatusFailed,
        (None, None) => return cx.throw_error("either status code or error message is required"),
    };

    // make an async block here so that it captures client, endpoint, and
    // http_result. it'd be nice to just pass `client.report_status(e, r)` but
    // that doesn't move anything into the capture, it just takes references.
    let client = client.cloned();
    let endpoint = endpoint.cloned();
    let report_status = async move { client.report_status(endpoint, http_result).await };

    Ok(
        runtime.spawn_promise(&mut cx, report_status, |mut cx, res| match res {
            Ok(endpoint) => js_endpoint(&mut cx, endpoint),
            Err(e) => cx.throw_error(e.to_string()),
        }),
    )
}

/// convert a JS string[][] to an http::HeaderMap
///
/// the standard HTTP headers object in js joins multiple values together and
/// stringifies them, so this array should have every element be a two-element
/// k,v pair of header name and stringified, joined header value.
fn headers_from_js<'a>(
    cx: &mut impl Context<'a>,
    headers: Handle<'_, JsArray>,
) -> NeonResult<http::HeaderMap> {
    let js_headers = headers.to_vec(cx)?;

    let mut headers = http::HeaderMap::with_capacity(js_headers.len());
    for entry in js_headers {
        let entry: Handle<JsArray> = entry.downcast_or_throw(cx)?;
        let k: Handle<JsString> = entry.get(cx, 0)?;
        let v: Handle<JsString> = entry.get(cx, 1)?;

        let k: http::HeaderName = from_js_str(cx, &k)?;
        let v: http::HeaderValue = from_js_str(cx, &v)?;
        headers.insert(k, v);
    }

    Ok(headers)
}

fn js_endpoint<'a>(
    cx: &mut impl Context<'a>,
    endpoint: junction_core::Endpoint,
) -> JsResult<'a, JsArray> {
    fn js_endpoint_obj<'a>(
        cx: &mut impl Context<'a>,
        endpoint: &junction_core::Endpoint,
    ) -> JsResult<'a, JsObject> {
        let sock_opts = cx.empty_object();
        let ip_addr = cx.string(endpoint.addr().ip().to_string());
        sock_opts.set(cx, "address", ip_addr)?;
        let port = cx.number(endpoint.addr().port());
        sock_opts.set(cx, "port", port)?;

        let ep = cx.empty_object();
        ep.set(cx, "sockAddr", sock_opts)?;

        let scheme = cx.string(endpoint.url().scheme());
        ep.set(cx, "scheme", scheme)?;

        let hostname = cx.string(endpoint.url().hostname());
        ep.set(cx, "hostname", hostname)?;

        if let Some(retry) = endpoint.retry() {
            let retry = js_retry(cx, retry)?;
            ep.set(cx, "retry", retry)?;
        }

        if let Some(timeouts) = endpoint.timeouts() {
            let timeouts = js_timeouts(cx, timeouts)?;
            ep.set(cx, "timeouts", timeouts)?;
        }

        Ok(ep)
    }

    let obj = js_endpoint_obj(cx, &endpoint)?;
    let handle = cx.boxed(ffi::Endpoint(endpoint));

    let values = JsArray::new(cx, 2);
    values.set(cx, 0, handle)?;
    values.set(cx, 1, obj)?;
    Ok(values)
}

fn js_retry<'a>(
    cx: &mut impl Context<'a>,
    retry: &junction_api::http::RouteRetry,
) -> JsResult<'a, JsObject> {
    let obj = cx.empty_object();

    let attempts: Handle<JsValue> = match &retry.attempts {
        Some(attempts) => cx.number(*attempts).upcast(),
        None => cx.undefined().upcast(),
    };
    obj.set(cx, "attempts", attempts)?;

    let backoff: Handle<JsValue> = match &retry.backoff {
        Some(backoff) => js_duration_ms(cx, backoff)?.upcast(),
        None => cx.undefined().upcast(),
    };
    obj.set(cx, "backoff", backoff)?;

    let retry_codes = JsArray::new(cx, retry.codes.len());
    for (i, &code) in retry.codes.iter().enumerate() {
        let code = cx.number(code);
        retry_codes.set(cx, i as u32, code)?;
    }
    obj.set(cx, "codes", retry_codes)?;

    Ok(obj)
}

fn js_timeouts<'a>(
    cx: &mut impl Context<'a>,
    timeouts: &junction_api::http::RouteTimeouts,
) -> JsResult<'a, JsObject> {
    let obj = cx.empty_object();

    let request: Handle<JsValue> = match &timeouts.request {
        Some(request) => js_duration_ms(cx, request)?.upcast(),
        None => cx.undefined().upcast(),
    };
    obj.set(cx, "request", request)?;

    let backend_request: Handle<JsValue> = match &timeouts.backend_request {
        Some(backend_request) => js_duration_ms(cx, backend_request)?.upcast(),
        None => cx.undefined().upcast(),
    };
    obj.set(cx, "backendRequest", backend_request)?;

    Ok(obj)
}

fn js_duration_ms<'a>(cx: &mut impl Context<'a>, d: &Duration) -> JsResult<'a, JsNumber> {
    let secs = d.as_secs();
    let subsec_nanos = d.subsec_nanos() as u64;
    let millis = secs * 1000 + (subsec_nanos / 1_000_000);
    Ok(cx.number(millis as f64))
}

/// A shared tokio Runtime that is shut down when finalized by the JS executor.
///
/// There's generally no reason not to share the same runtime between all tasks
/// but to avoid contention with JS worker threads, the typescript layer on top
/// of the FFI code is responsible for managing which runtimes exist.
struct Runtime {
    // TODO: it's probably better to share a channel here that we use to pass
    // tasks back to the JS thread afterwards, but we need some way to unref
    // it when there's no pending work. having folks call Runtime.close() is
    // a non-starter.
    runtime: Arc<tokio::runtime::Runtime>,
}

/// when finalized, shut down the Runtime without waiting for any background
/// work to complete. do this to avoid blocking any JS worker threads - anything
/// we spawn should be tolerant of a shutdown runtime.
impl Finalize for Runtime {
    fn finalize<'a, C: Context<'a>>(self, _: &mut C) {
        if let Some(inner) = Arc::into_inner(self.runtime) {
            inner.shutdown_background();
        }
    }
}

impl Runtime {
    fn new_default() -> Result<Self, std::io::Error> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("junction-core")
            .worker_threads(2)
            .enable_all()
            .build()?;

        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    fn spawn_promise<'a, F, S, V>(
        &self,
        cx: &mut impl Context<'a>,
        fut: F,
        settle: S,
    ) -> Handle<'a, JsPromise>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        S: FnOnce(TaskContext, F::Output) -> JsResult<V> + Send + 'static,
        V: Value,
    {
        let (deferred, promise) = cx.promise();
        let channel = cx.channel();
        self.runtime.spawn(async move {
            let output = fut.await;

            // try to send the result back to the main thread. if something goes
            // wrong that's not our problem - the runtime is sad for some reason
            // and probably shutting down.  swallow the error and move on.
            let _ = deferred.try_settle_with(&channel, |cx| settle(cx, output));
        });

        promise
    }
}

mod ffi {
    use super::*;

    /// Create a Node FFI wrapper type around an existing type.
    ///
    /// The wrapper type implements a no-op Finalize and provides some
    /// convenience methods for getting the wrapped value by ref and
    /// by value.
    macro_rules! js_wrapper {
        ($ty_name:ident, $inner:ty) => {
            pub(super) struct $ty_name(pub(super) $inner);

            impl Finalize for $ty_name {}

            impl $ty_name {
                pub(super) fn cloned(&self) -> $inner {
                    self.0.clone()
                }
            }
        };
    }

    js_wrapper!(Client, junction_core::Client);
    js_wrapper!(Endpoint, junction_core::Endpoint);
}
