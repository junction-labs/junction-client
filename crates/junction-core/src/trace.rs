use std::{net::SocketAddr, time::Instant};

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
