//! A structured resolution and reporting trace. This is framework/platform
//! agnostic so that language clients can integrate with their telemetry of
//! choice.
//!
//! # A note on Time
//!
//! This struct uses a SystemTime internally for timing to give each event
//! a sane global timestamp in the context of the rest of the system. This
//! tends to use `CLOCK_REALTIME` instead of `CLOCK_MONOTONIC` on linux/unix
//! with all of the timing drawbacks that implies.
//!
//! This is the same tradeoff that otel and tracing have to make:
//! <https://github.com/open-telemetry/opentelemetry-rust/blob/2bf8175d071232eb3667171f2cd8f1eb9324fada/opentelemetry/src/lib.rs#L284>

use std::{fmt::Display, net::SocketAddr, time::SystemTime};

use smol_str::{SmolStr, ToSmolStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TracePhase {
    RouteResolution,
    EndpointSelection(u8),
}

impl Display for TracePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TracePhase::RouteResolution => write!(f, "RouteResolution"),
            TracePhase::EndpointSelection(n) => write!(f, "EndpointSelection({n})"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
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

impl Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventKind::LookupListener => write!(f, "LookupListener"),
            EventKind::LookupRoute => write!(f, "LookupRoute"),
            EventKind::MatchRoute => write!(f, "MatchRoute"),
            EventKind::SelectCluster => write!(f, "SelectCluster"),
            EventKind::HashRequest => write!(f, "HashRequest"),
            EventKind::LookupCluster => write!(f, "LookupCluster"),
            EventKind::LookupEndpoints => write!(f, "LookupEndpoints"),
            EventKind::LookupDns => write!(f, "LookupDns"),
            EventKind::LoadBalance => write!(f, "LoadBalance"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    phase: TracePhase,
    kind: EventKind,
    time: SystemTime,
    fields: Vec<TraceData>,
}

impl Event {
    pub fn phase(&self) -> TracePhase {
        self.phase
    }

    pub fn kind(&self) -> EventKind {
        self.kind
    }

    pub fn time(&self) -> SystemTime {
        self.time
    }

    pub fn fields(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.fields
            .iter()
            .map(|data| (data.name, data.value.as_ref()))
    }
}

#[derive(Clone, Debug)]
pub struct Trace {
    start: SystemTime,
    phase: TracePhase,
    events: Vec<Event>,
}

impl Trace {
    pub fn start(&self) -> SystemTime {
        self.start
    }

    pub fn events(&self) -> impl Iterator<Item = &Event> + '_ {
        self.events.iter()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TraceData {
    name: &'static str,
    value: SmolStr,
}

impl TraceData {
    fn new<T: ToSmolStr>(name: &'static str, value: T) -> Self {
        Self {
            name,
            value: value.to_smolstr(),
        }
    }
}

impl Trace {
    pub(crate) fn new() -> Self {
        Trace {
            start: SystemTime::now(),
            phase: TracePhase::RouteResolution,
            events: Vec::new(),
        }
    }

    // Route Resolution builders

    pub(crate) fn lookup_listener(&mut self, listener: String) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(Event {
            kind: EventKind::LookupListener,
            phase: TracePhase::RouteResolution,
            time: SystemTime::now(),
            fields: vec![TraceData::new("listener", listener)],
        })
    }

    pub(crate) fn lookup_route(&mut self, route: String) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(Event {
            kind: EventKind::LookupRoute,
            phase: TracePhase::RouteResolution,
            time: SystemTime::now(),
            fields: vec![TraceData::new("route", route)],
        })
    }

    pub(crate) fn matched_route(&mut self) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(Event {
            kind: EventKind::MatchRoute,
            phase: TracePhase::RouteResolution,
            time: SystemTime::now(),
            fields: vec![],
        })
    }

    pub(crate) fn select_cluster(&mut self) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(Event {
            phase: self.phase,
            kind: EventKind::SelectCluster,
            time: SystemTime::now(),
            fields: vec![],
        });
    }

    pub(crate) fn hash_request(&mut self, request_hash: u64) {
        debug_assert!(matches!(self.phase, TracePhase::RouteResolution));

        self.events.push(Event {
            phase: self.phase,
            kind: EventKind::HashRequest,
            time: SystemTime::now(),
            fields: vec![TraceData::new("request-hash", request_hash)],
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
        self.events.push(Event {
            phase: self.phase,
            kind: EventKind::LookupCluster,
            time: SystemTime::now(),
            fields: vec![TraceData::new("cluster", cluster)],
        });
    }

    pub(crate) fn lookup_endpoints(&mut self, endpoints: String) {
        self.events.push(Event {
            phase: self.phase,
            kind: EventKind::LookupEndpoints,
            time: SystemTime::now(),
            fields: vec![TraceData::new("endpoints", endpoints)],
        });
    }

    pub(crate) fn lookup_dns(&mut self, hostname: String) {
        self.events.push(Event {
            phase: self.phase,
            kind: EventKind::LookupDns,
            time: SystemTime::now(),
            fields: vec![TraceData::new("hostname", hostname)],
        });
    }

    pub(crate) fn load_balance<T: ToSmolStr>(
        &mut self,
        lb_name: &'static str,
        addr: Option<&SocketAddr>,
        extra: impl IntoIterator<Item = (&'static str, T)>,
    ) {
        debug_assert!(matches!(self.phase, TracePhase::EndpointSelection(_)));

        let extra = extra.into_iter().map(|(k, v)| TraceData::new(k, v));
        let (extra_len, _) = extra.size_hint();

        let mut fields = Vec::with_capacity(2 + extra_len);
        fields.push(TraceData::new("type", lb_name));
        fields.push(TraceData::new(
            "addr",
            addr.map(|a| a.to_smolstr())
                .unwrap_or_else(|| SmolStr::new_static("")),
        ));
        fields.extend(extra);

        self.events.push(Event {
            kind: EventKind::LoadBalance,
            phase: self.phase,
            time: SystemTime::now(),
            fields,
        });
    }
}
