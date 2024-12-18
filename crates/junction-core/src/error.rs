use std::{borrow::Cow, net::SocketAddr, time::Instant};

use junction_api::{backend::BackendId, http::Route, Name};
use smol_str::{SmolStr, ToSmolStr};

#[derive(Clone, Debug)]
pub(crate) struct Trace {
    start: Instant,
    phase: TracePhase,
    events: Vec<TraceEvent>,
}

#[derive(Clone, Debug)]
pub(crate) struct TraceEvent {
    pub(crate) phase: TracePhase,
    pub(crate) kind: TraceEventKind,
    pub(crate) at: Instant,
    pub(crate) kv: Vec<TraceData>,
}

type TraceData = (&'static str, SmolStr);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TracePhase {
    RouteResolution,
    EndpointSelection(u8),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum TraceEventKind {
    RouteLookup,
    RouteRuleMatched,
    BackendSelected,
    BackendLookup,
    EndpointsLookup,
    SelectAddr,
}

impl Trace {
    pub(crate) fn new() -> Self {
        Trace {
            start: Instant::now(),
            phase: TracePhase::RouteResolution,
            events: Vec::new(),
        }
    }

    pub(crate) fn events(&self) -> impl Iterator<Item = &TraceEvent> {
        self.events.iter()
    }

    pub(crate) fn start(&self) -> Instant {
        self.start
    }

    pub(crate) fn lookup_route(&mut self, route: &Route) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(TraceEvent {
            kind: TraceEventKind::RouteLookup,
            phase: TracePhase::RouteResolution,
            at: Instant::now(),
            kv: vec![("route", route.id.to_smolstr())],
        })
    }

    pub(crate) fn matched_rule(&mut self, rule: usize, rule_name: Option<&Name>) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        let kv = match rule_name {
            Some(name) => vec![("rule-name", name.to_smolstr())],
            None => vec![("rule-idx", rule.to_smolstr())],
        };

        self.events.push(TraceEvent {
            kind: TraceEventKind::RouteRuleMatched,
            phase: TracePhase::RouteResolution,
            at: Instant::now(),
            kv,
        })
    }

    pub(crate) fn select_backend(&mut self, backend: &BackendId) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(TraceEvent {
            phase: self.phase,
            kind: TraceEventKind::BackendSelected,
            at: Instant::now(),
            kv: vec![("name", backend.to_smolstr())],
        });
    }

    pub(crate) fn start_endpoint_selection(&mut self) {
        let next_phase = match self.phase {
            TracePhase::RouteResolution => TracePhase::EndpointSelection(0),
            TracePhase::EndpointSelection(n) => TracePhase::EndpointSelection(n + 1),
        };
        self.phase = next_phase;
    }

    pub(crate) fn lookup_backend(&mut self, backend: &BackendId) {
        debug_assert!(matches!(self.phase, TracePhase::EndpointSelection(_)));

        self.events.push(TraceEvent {
            kind: TraceEventKind::BackendLookup,
            phase: self.phase,
            at: Instant::now(),
            kv: vec![("backend-id", backend.to_smolstr())],
        })
    }

    pub(crate) fn lookup_endpoints(&mut self, backend: &BackendId) {
        debug_assert!(matches!(self.phase, TracePhase::EndpointSelection(_)));

        self.events.push(TraceEvent {
            kind: TraceEventKind::EndpointsLookup,
            phase: self.phase,
            at: Instant::now(),
            kv: vec![("backend-id", backend.to_smolstr())],
        })
    }

    pub(crate) fn load_balance(
        &mut self,
        lb_name: &'static str,
        addr: Option<&SocketAddr>,
        extra: Vec<TraceData>,
    ) {
        debug_assert!(matches!(self.phase, TracePhase::EndpointSelection(_)));

        let mut kv = Vec::with_capacity(extra.len() + 2);
        kv.push(("type", lb_name.to_smolstr()));
        kv.push((
            "addr",
            addr.map(|a| a.to_smolstr())
                .unwrap_or_else(|| "-".to_smolstr()),
        ));
        kv.extend(extra);

        self.events.push(TraceEvent {
            kind: TraceEventKind::SelectAddr,
            phase: self.phase,
            at: Instant::now(),
            kv,
        });
    }
}

/// A `Result` alias where the `Err` case is `junction_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

/// An error when using the Junction client.
#[derive(Debug, thiserror::Error)]
#[error("{inner}")]
pub struct Error {
    // a trace of what's happened so far
    trace: Option<Trace>,

    // boxed to keep the size of the error down. this apparently has a large
    // effect on the performance of calls to functions that return
    // Result<_, Error>.
    //
    // https://rust-lang.github.io/rust-clippy/master/index.html#result_large_err
    // https://docs.rs/serde_json/latest/src/serde_json/error.rs.html#15-20
    inner: Box<ErrorImpl>,
}

impl Error {
    /// Returns `true` if this is a temporary error.
    ///
    /// Temporary errors may occur because of a network timeout or because of
    /// lag fetching a configuration from a Junction server.
    pub fn is_temporary(&self) -> bool {
        matches!(*self.inner, ErrorImpl::NoReachableEndpoints { .. })
    }
}

impl Error {
    // timeouts

    // FIXME: needs a trace?
    pub(crate) fn timed_out(message: &'static str) -> Self {
        let inner = ErrorImpl::TimedOut(Cow::from(message));
        Self {
            trace: None,
            inner: Box::new(inner),
        }
    }

    // url problems
    //
    // TODO: should this be a separate type? thye don't need a Trace or anything

    pub(crate) fn into_invalid_url(message: String) -> Self {
        let inner = ErrorImpl::InvalidUrl(Cow::Owned(message));
        Self {
            trace: None,
            inner: Box::new(inner),
        }
    }

    pub(crate) fn invalid_url(message: &'static str) -> Self {
        let inner = ErrorImpl::InvalidUrl(Cow::Borrowed(message));
        Self {
            trace: None,
            inner: Box::new(inner),
        }
    }

    // route problems

    pub(crate) fn no_route_matched(authority: String, trace: Trace) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::NoRouteMatched { authority }),
        }
    }

    pub(crate) fn no_rule_matched(route: Name, trace: Trace) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::NoRuleMatched { route }),
        }
    }

    pub(crate) fn invalid_route(
        message: &'static str,
        id: Name,
        rule: usize,
        trace: Trace,
    ) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::InvalidRoute { id, message, rule }),
        }
    }

    // backend problems

    pub(crate) fn no_backend(backend: BackendId, trace: Trace) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::NoBackend { backend }),
        }
    }

    pub(crate) fn no_reachable_endpoints(backend: BackendId, trace: Trace) -> Self {
        Self {
            trace: Some(trace),
            inner: Box::new(ErrorImpl::NoReachableEndpoints { backend }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ErrorImpl {
    #[error("timed out: {0}")]
    TimedOut(Cow<'static, str>),

    #[error("invalid url: {0}")]
    InvalidUrl(Cow<'static, str>),

    #[error("invalid route configuration")]
    InvalidRoute {
        message: &'static str,
        id: Name,
        rule: usize,
    },

    #[error("no route matched: '{authority}'")]
    NoRouteMatched { authority: String },

    #[error("{route}: no rules matched the request")]
    NoRuleMatched { route: Name },

    #[error("{backend}: backend not found")]
    NoBackend { backend: BackendId },

    #[error("{backend}: no reachable endpoints")]
    NoReachableEndpoints { backend: BackendId },
}
