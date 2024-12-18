use crate::{endpoints::EndpointGroup, error::Trace, hash::thread_local_xxhash};
use junction_api::backend::{Backend, LbPolicy, RequestHashPolicy, RequestHasher, RingHashParams};
use smol_str::ToSmolStr;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    },
};

/// A [Backend] and the [LoadBalancer] it's configured with.
#[derive(Debug)]
pub struct BackendLb {
    pub config: Backend,
    pub load_balancer: LoadBalancer,
}

#[derive(Debug)]
pub enum LoadBalancer {
    RoundRobin(RoundRobinLb),
    RingHash(RingHashLb),
}

impl LoadBalancer {
    pub(crate) fn load_balance<'e>(
        &self,
        trace: &mut Trace,
        endpoints: &'e EndpointGroup,
        url: &crate::Url,
        headers: &http::HeaderMap,
        previous_addrs: &[SocketAddr],
    ) -> Option<&'e SocketAddr> {
        match self {
            // RoundRobin skips previously picked addrs and ignores context
            LoadBalancer::RoundRobin(lb) => lb.pick_endpoint(trace, endpoints, previous_addrs),
            // RingHash needs context but doesn't care about history
            LoadBalancer::RingHash(lb) => {
                lb.pick_endpoint(trace, endpoints, url, headers, &lb.config.hash_params)
            }
        }
    }
}

impl LoadBalancer {
    pub(crate) fn from_config(config: &LbPolicy) -> Self {
        match config {
            LbPolicy::RoundRobin => LoadBalancer::RoundRobin(RoundRobinLb::default()),
            LbPolicy::RingHash(x) => LoadBalancer::RingHash(RingHashLb::new(x)),
            LbPolicy::Unspecified => LoadBalancer::RoundRobin(RoundRobinLb::default()),
        }
    }
}

// TODO: when doing weighted round robin, it's worth adapting the GRPC
// scheduling in static_stride_scheduler.cc instead of inventing a new technique
// ourselves.
//
// src/core/load_balancing/weighted_round_robin/static_stride_scheduler.cc
#[derive(Debug, Default)]
pub struct RoundRobinLb {
    idx: AtomicUsize,
}

impl RoundRobinLb {
    // FIXME: actually use locality
    fn pick_endpoint<'e>(
        &self,
        trace: &mut Trace,
        endpoint_group: &'e EndpointGroup,
        previous_addrs: &[SocketAddr],
    ) -> Option<&'e SocketAddr> {
        // TODO: actually use previous addrs to pick a new address. have to
        // decide if we return anything if all addresses have previously been
        // picked, or how many dupes we allow, etc. Envoy has a policy for this,
        // but we'd prefer to do something simpler by default.
        //
        // https://www.envoyproxy.io/docs/envoy/latest/api-v3/config/route/v3/route_components.proto#config-route-v3-retrypolicy
        let _ = previous_addrs;

        let idx = self.idx.fetch_add(1, Ordering::SeqCst) % endpoint_group.len();
        let addr = endpoint_group.nth(idx);

        trace.load_balance("ROUND_ROBIN", addr, Vec::new());

        addr
    }
}

/// A ring hash LB using Ketama hashing, roughly compatible with the GRPC and
/// Envoy implementations.
///
/// Like the Envoy and gRPC implementations, this load balancer ignores locality
/// and flattens all visible endpoints into a single hash ring. Unlike GRPC and
/// Envoy, this load balancer ignores endpoint weights.
///
#[derive(Debug)]
pub struct RingHashLb {
    config: RingHashParams,
    ring: RwLock<Ring>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RingEntry {
    hash: u64,
    idx: usize,
}

impl RingHashLb {
    fn new(config: &RingHashParams) -> Self {
        Self {
            config: config.clone(),
            ring: RwLock::new(Ring {
                eg_hash: 0,
                entries: Vec::with_capacity(config.min_ring_size as usize),
            }),
        }
    }

    fn pick_endpoint<'e>(
        &self,
        trace: &mut Trace,
        endpoints: &'e EndpointGroup,
        url: &crate::Url,
        headers: &http::HeaderMap,
        hash_params: &Vec<RequestHashPolicy>,
    ) -> Option<&'e SocketAddr> {
        let request_hash =
            hash_request(hash_params, url, headers).unwrap_or_else(crate::rand::random);

        let endpoint_idx = self.with_ring(endpoints, |r| r.pick(request_hash))?;
        let addr = endpoints.nth(endpoint_idx);

        trace.load_balance("RING_HASH", addr, vec![("hash", request_hash.to_smolstr())]);

        addr
    }

    // if you're reading this you might wonder why this takes a callback:
    //
    // the answer is that std's RWLock isn't upgradeable or downgradeable, so
    // it's not easy to have an RAII guard that starts with a read lock and
    // transparently upgrades to write when you need mut access.
    //
    // instead of figuring that out, this fn does the read to write upgrade and
    // you pass the callback. easy peasy.
    fn with_ring<F, T>(&self, endpoint_group: &EndpointGroup, mut cb: F) -> T
    where
        F: FnMut(&Ring) -> T,
    {
        // try to just use the existing ring
        //
        // explicitly drop the guard at the end so we can't get confused and
        // try to upgrade and deadlock ourselves.
        let ring = self.ring.read().unwrap();
        if ring.eg_hash == endpoint_group.hash {
            return cb(&ring);
        }
        std::mem::drop(ring);

        // write path:
        let mut ring = self.ring.write().unwrap();
        ring.rebuild(self.config.min_ring_size as usize, endpoint_group);
        cb(&ring)
    }
}

#[derive(Debug)]
struct Ring {
    // The hash of the EndpointGroup used to build the Ring. This is slightly
    // more stable than ResourceVersion, but could be changed to that.
    eg_hash: u64,
    entries: Vec<RingEntry>,
}

impl Ring {
    fn rebuild(&mut self, min_size: usize, endpoint_group: &EndpointGroup) {
        // before factoring in weights, we're doing some quick math to get a
        // multiple of the endpoint count size to fill the ring.
        //
        // once we factor in weights, this is going to get way nastier.
        let endpoint_count = endpoint_group.len();
        let repeats = usize::max((min_size as f64 / endpoint_count as f64).ceil() as usize, 1);
        let ring_size = repeats * endpoint_count;

        self.entries.clear();
        self.entries.reserve(ring_size);

        for (idx, endpoint) in endpoint_group.iter().enumerate() {
            for i in 0..repeats {
                let hash = thread_local_xxhash::hash(&(endpoint, i));
                self.entries.push(RingEntry { hash, idx });
            }
        }

        self.eg_hash = endpoint_group.hash;
        self.entries.sort_by_key(|e| e.hash);
    }

    fn pick(&self, endpoint_hash: u64) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }

        // the envoy/grpc implementations use a binary search cago culted from
        // ketama_get_server in the original ketama implementation.
        //
        // instead of doing that, use the stdlib. partition_point returns the
        // first idx for which the endpoint hash is larger than the
        // endpoint_hash using a binary search.
        let entry_idx = self.entries.partition_point(|e| e.hash < endpoint_hash);
        let entry_idx = entry_idx % self.entries.len();
        Some(self.entries[entry_idx].idx)
    }
}

/// Hash an outgoing request based on a set of hash policies.
///
/// Like Envoy and gRPC, multiple hash policies are combined by applying a
/// bitwise left-rotate to the previous value and xor-ing the new value into
/// the previous value.
///
/// See:
/// - https://github.com/grpc/proposal/blob/master/A42-xds-ring-hash-lb-policy.md#xds-api-fields
/// - https://github.com/envoyproxy/envoy/blob/main/source/common/http/hash_policy.cc#L236-L257
pub(crate) fn hash_request(
    hash_policies: &Vec<RequestHashPolicy>,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Option<u64> {
    let mut hash: Option<u64> = None;

    for hash_policy in hash_policies {
        if let Some(new_hash) = hash_component(hash_policy, url, headers) {
            hash = Some(match hash {
                Some(hash) => hash.rotate_left(1) ^ new_hash,
                None => new_hash,
            });

            if hash_policy.terminal {
                break;
            }
        }
    }

    hash
}

fn hash_component(
    policy: &RequestHashPolicy,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Option<u64> {
    match &policy.hasher {
        RequestHasher::Header { name } => {
            let mut header_values: Vec<_> = headers
                .get_all(name)
                .iter()
                .map(http::HeaderValue::as_bytes)
                .collect();

            if header_values.is_empty() {
                None
            } else {
                // sort values so that "foo,bar" and "bar,foo" hash to the same value
                header_values.sort();
                Some(thread_local_xxhash::hash_iter(header_values))
            }
        }
        RequestHasher::QueryParam { ref name } => url.query().map(|query| {
            let matching_vals = form_urlencoded::parse(query.as_bytes())
                .filter_map(|(param, value)| (&param == name).then_some(value));
            thread_local_xxhash::hash_iter(matching_vals)
        }),
    }
}

#[cfg(test)]
mod test_ring_hash {
    use crate::endpoints::Locality;

    use super::*;

    #[test]
    fn test_rebuild_ring() {
        let mut ring = Ring {
            eg_hash: 0,
            entries: Vec::new(),
        };

        // rebuild a ring with no min size
        ring.rebuild(
            0,
            &EndpointGroup::new(
                [(
                    Locality::Unknown,
                    vec!["1.1.1.1:80".parse().unwrap(), "1.1.1.2:80".parse().unwrap()],
                )]
                .into(),
            ),
        );

        assert_eq!(ring.eg_hash, 7513761254796112512);
        assert_eq!(ring.entries.len(), 2);
        assert_eq!(ring_indexes(&ring), (0..2).collect::<Vec<_>>());
        assert_hashes_unique(&ring);

        let first_ring = ring.entries.clone();

        // ignore a rebuild with the same hash
        ring.rebuild(
            0,
            &EndpointGroup::new(
                [(
                    Locality::Unknown,
                    vec![
                        "1.1.1.1:80".parse().unwrap(),
                        "1.1.1.2:80".parse().unwrap(),
                        "1.1.1.3:80".parse().unwrap(),
                    ],
                )]
                .into(),
            ),
        );

        assert_eq!(ring.eg_hash, 14133933280653238819);
        assert_eq!(ring.entries.len(), 3);
        assert_eq!(ring_indexes(&ring), (0..3).collect::<Vec<_>>());
        assert_hashes_unique(&ring);

        // the ring with the entry for the new address (1.1.1.3) removed should
        // be the same as the ring for the prior group.
        let second_ring: Vec<_> = ring
            .entries
            .iter()
            .filter(|e| e.idx != 2)
            .cloned()
            .collect();
        assert_eq!(first_ring, second_ring);
    }

    #[test]
    fn test_rebuild_ring_min_size() {
        let mut ring = Ring {
            eg_hash: 0,
            entries: Vec::new(),
        };

        // rebuild a ring with no min size
        ring.rebuild(
            1024,
            &EndpointGroup::new(
                [(
                    Locality::Unknown,
                    vec![
                        "1.1.1.1:80".parse().unwrap(),
                        "1.1.1.2:80".parse().unwrap(),
                        "1.1.1.3:80".parse().unwrap(),
                    ],
                )]
                .into(),
            ),
        );

        // 1026 is the largest multiple of 3 larger than 1024
        assert_eq!(ring.entries.len(), 1026);

        // every idx should be repeated the 1026/3 times.
        let mut counts = [0usize; 3];
        for entry in &ring.entries {
            counts[entry.idx] += 1;
        }
        assert!(counts.iter().all(|&c| c == 342));

        // every hash should be unique
        assert_hashes_unique(&ring);
    }

    #[test]
    fn test_pick() {
        let mut ring = Ring {
            eg_hash: 0,
            entries: vec![],
        };
        ring.rebuild(
            0,
            &EndpointGroup::new(
                [(
                    Locality::Unknown,
                    vec![
                        "1.1.1.1:80".parse().unwrap(),
                        "1.1.1.2:80".parse().unwrap(),
                        "1.1.1.3:80".parse().unwrap(),
                    ],
                )]
                .into(),
            ),
        );

        // anything less than or eq to the first hash, or greater than the last
        // hash should hash to the start the ring.
        let hashes_to_first = [0, ring.entries[0].hash, ring.entries[2].hash + 1];
        for hash in hashes_to_first {
            assert_eq!(ring.pick(hash), Some(ring.entries[0].idx),)
        }

        let hashes_to_last = [ring.entries[2].hash - 1, ring.entries[2].hash];
        for hash in hashes_to_last {
            assert_eq!(ring.pick(hash), Some(ring.entries[2].idx));
        }
    }

    fn ring_indexes(r: &Ring) -> Vec<usize> {
        let mut indexes: Vec<_> = r.entries.iter().map(|e| e.idx).collect();
        indexes.sort();
        indexes
    }

    fn assert_hashes_unique(r: &Ring) {
        let mut hashes: Vec<_> = r.entries.iter().map(|e| e.hash).collect();
        hashes.sort();
        hashes.dedup();
        assert_eq!(hashes.len(), r.entries.len());
    }
}
