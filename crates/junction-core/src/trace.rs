use std::{net::SocketAddr, time::Instant};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TraceEventKind {
    // RouteResolution
    LookupListener,
    LookupRoute,
    MatchRoute,
    SelectCluster,
    HashRequest,
    // EndpointSelection
    LookupCluster,
    LookupEndpoints,
    LookupDns,
    LoadBalance,
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

    // Route Resolution builders

    pub(crate) fn lookup_listener(&mut self, listener: String) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(TraceEvent {
            kind: TraceEventKind::LookupListener,
            phase: TracePhase::RouteResolution,
            at: Instant::now(),
            kv: vec![("listener", listener.to_smolstr())],
        })
    }

    pub(crate) fn lookup_route(&mut self, route: String) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(TraceEvent {
            kind: TraceEventKind::LookupRoute,
            phase: TracePhase::RouteResolution,
            at: Instant::now(),
            kv: vec![("route", route.to_smolstr())],
        })
    }

    pub(crate) fn matched_route(&mut self) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(TraceEvent {
            kind: TraceEventKind::MatchRoute,
            phase: TracePhase::RouteResolution,
            at: Instant::now(),
            kv: vec![],
        })
    }

    pub(crate) fn select_cluster(&mut self) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(TraceEvent {
            phase: self.phase,
            kind: TraceEventKind::SelectCluster,
            at: Instant::now(),
            kv: vec![],
        });
    }

    pub(crate) fn hash_request(&mut self, request_hash: u64) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(TraceEvent {
            phase: self.phase,
            kind: TraceEventKind::HashRequest,
            at: Instant::now(),
            kv: vec![("request-hash", request_hash.to_smolstr())],
        });
    }

    // Endpoint Selection builders

    pub(crate) fn start_endpoint_selection(&mut self) {
        let next_phase = match self.phase {
            TracePhase::RouteResolution => TracePhase::EndpointSelection(0),
            TracePhase::EndpointSelection(n) => TracePhase::EndpointSelection(n + 1),
        };
        self.phase = next_phase;
    }

    pub(crate) fn lookup_cluster(&mut self, cluster: String) {
        self.events.push(TraceEvent {
            phase: self.phase,
            kind: TraceEventKind::LookupCluster,
            at: Instant::now(),
            kv: vec![("cluster", cluster.to_smolstr())],
        });
    }

    pub(crate) fn lookup_endpoints(&mut self, endpoints: String) {
        self.events.push(TraceEvent {
            phase: self.phase,
            kind: TraceEventKind::LookupEndpoints,
            at: Instant::now(),
            kv: vec![("endpoints", endpoints.to_smolstr())],
        });
    }

    pub(crate) fn lookup_dns(&mut self, hostname: String) {
        self.events.push(TraceEvent {
            phase: self.phase,
            kind: TraceEventKind::LookupDns,
            at: Instant::now(),
            kv: vec![("hostname", hostname.to_smolstr())],
        });
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
            kind: TraceEventKind::LoadBalance,
            phase: self.phase,
            at: Instant::now(),
            kv,
        });
    }
}
