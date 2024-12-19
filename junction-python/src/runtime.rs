use std::{future::Future, time::Duration};

use once_cell::sync::Lazy;
use pyo3::{exceptions::PyRuntimeError, PyErr, PyResult, Python};

static RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("junction")
        .build()
        .expect("Junction failed to initialize its async runtime. this is a bug in Junction");

    rt
});

/// Spawn a task on a static/lazy tokio runtime.
///
/// Python and Bound are !Send. Do not try to work around this - holding the GIL
/// in a background task WILL cause deadlocks.
pub(crate) fn spawn<F, T>(fut: F) -> tokio::task::JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    RUNTIME.spawn(fut)
}

/// Acquire a static/lazy tokio Runtime and `block_on` a future while
/// occasionally allowing the interpreter to check for signals.
///
/// This fn always converts the error type of the future to a PyRuntimeError by
/// calling `PyRuntimeError::new_err(e.to_string())`
///
/// # You're on main
///
/// Checking for signals is done on the current thread, as guaranteed by
/// [tokio::runtime::Runtime::block_on], HOWEVER, checking for signals from any
/// thread but the main thread will do nothing. Calling this fn on any thread
/// but the main fn will do nothing but incur the overhead of running an
/// unnecessary future.
///
/// Checking signals requires holding the GIL, which would be a bad thing to do
/// across an await point - instead of taking a `Python<'_>` token, it
/// periodically calls [Python::with_gil] and briefly holds the GIL to check on
/// signals. See the [Python] docs for more about the GIL and deadlocks.
///
/// # (Not) Holding the GIL
///
/// In addition to the signal check not holding the GIL, you should ALSO not be
/// holding the GIL while calling this fn. The future passed to this fn and its
/// outputs must be Send, which means the compiler will keep you from passing a
/// Python or a Bound here.
///
/// HOWEVER, the caller might implicitly have a Python or a Bound in scope, so
/// before calling block_on this function (re)acquires the GIL so that it can
/// temporarily suspend it while `block_on` runs.
///
/// The Pyo3 authors recommend a slightly different, finer-grained which will
/// appears to release the GIL while a future is being Polled but not while it
/// is suspended waiting for its next poll.
///
/// https://pyo3.rs/v0.23.3/async-await.html#release-the-gil-across-await
pub(crate) fn block_and_check_signals<F, T, E>(fut: F) -> PyResult<T>
where
    F: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display,
{
    async fn check_signals() -> PyErr {
        loop {
            if let Err(e) = Python::with_gil(|py| py.check_signals()) {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    Python::with_gil(|py| {
        py.allow_threads(|| {
            RUNTIME.block_on(async {
                tokio::select! {
                    biased;
                    res = fut => res.map_err(|e| PyRuntimeError::new_err(e.to_string())),
                    e = check_signals() => Err(e),
                }
            })
        })
    })
}
