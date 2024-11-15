use crate::EndpointAddress;
use junction_api::backend::{
    Backend, LbPolicy, RingHashParams, SessionAffinityHashParam, SessionAffinityHashParamType,
};
use std::{
    collections::BTreeMap,
    hash::Hash,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    },
};

// FIXME: we ignore weights in EndpointGroup. that probably shouldn't be the case

#[derive(Debug, Default, Hash, PartialEq, Eq)]
pub(crate) struct EndpointGroup {
    hash: u64,
    endpoints: BTreeMap<Locality, Vec<crate::EndpointAddress>>,
}

impl EndpointGroup {
    pub(crate) fn new(endpoints: BTreeMap<Locality, Vec<crate::EndpointAddress>>) -> Self {
        let hash = thread_local_xxhash::hash(&endpoints);
        Self { hash, endpoints }
    }

    pub(crate) fn from_dns_addrs(addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
        let mut endpoints = BTreeMap::new();
        let endpoint_addrs = addrs.into_iter().map(EndpointAddress::from).collect();
        endpoints.insert(Locality::Unknown, endpoint_addrs);

        Self::new(endpoints)
    }

    pub(crate) fn len(&self) -> usize {
        self.endpoints.values().map(|v| v.len()).sum()
    }

    /// Returns an iterator over all endpoints in the group.
    ///
    /// Iteration order is guaranteed to be stable as long as the EndpointGroup is
    /// not modified, and guaranteed to consecutively produce all addresses in a single
    /// locality.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &EndpointAddress> {
        self.endpoints.values().flatten()
    }

    /// Return the nth address in this group. The order
    pub(crate) fn nth(&self, n: usize) -> Option<&EndpointAddress> {
        let mut n = n;
        for endpoints in self.endpoints.values() {
            if n < endpoints.len() {
                return Some(&endpoints[n]);
            }
            n -= endpoints.len();
        }

        None
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Locality {
    Unknown,
    #[allow(unused)]
    Known(LocalityInfo),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LocalityInfo {
    pub(crate) region: String,
    pub(crate) zone: String,
}

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
    pub(crate) fn load_balance<'ep>(
        &self,
        url: &crate::Url,
        headers: &http::HeaderMap,
        locality_endpoints: &'ep EndpointGroup,
    ) -> Option<&'ep crate::EndpointAddress> {
        match self {
            LoadBalancer::RoundRobin(lb) => lb.pick_endpoint(locality_endpoints),
            LoadBalancer::RingHash(lb) => {
                lb.pick_endpoint(url, headers, &lb.config.hash_params, locality_endpoints)
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
    fn pick_endpoint<'e>(&self, endpoint_group: &'e EndpointGroup) -> Option<&'e EndpointAddress> {
        let endpoints = endpoint_group.endpoints.get(&Locality::Unknown)?;
        let locality_idx = self.idx.fetch_add(1, Ordering::SeqCst) % endpoints.len();
        Some(&endpoints[locality_idx])
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
                endpoint_group_hash: 0,
                entries: Vec::with_capacity(config.min_ring_size as usize),
            }),
        }
    }

    fn pick_endpoint<'e>(
        &self,
        url: &crate::Url,
        headers: &http::HeaderMap,
        hash_params: &Vec<SessionAffinityHashParam>,
        endpoint_group: &'e EndpointGroup,
    ) -> Option<&'e EndpointAddress> {
        let request_hash =
            hash_request(hash_params, url, headers).unwrap_or_else(crate::rand::random);

        let endpoint_idx = self.with_ring(endpoint_group, |r| r.pick(request_hash))?;
        endpoint_group.nth(endpoint_idx)
    }

    fn with_ring<F, T>(&self, endpoint_group: &EndpointGroup, mut cb: F) -> T
    where
        F: FnMut(&Ring) -> T,
    {
        // std's rwlocks aren't ugpradeable or downgradeable, so if we have to
        // recreate the ring, there's no way to go from having modified the
        // ring to using the version we just built. instead of figuring out
        // how to return impl<Deref<Target = Ring>> or whatever, just accept
        // a callback.
        let ring = self.ring.read().unwrap();
        if ring.endpoint_group_hash == endpoint_group.hash {
            return cb(&ring);
        }

        std::mem::drop(ring);
        let mut ring = self.ring.write().unwrap();
        ring.rebuild(self.config.min_ring_size as usize, endpoint_group);
        cb(&ring)
    }
}

// FIXME: this completely ignores backend weights.
#[derive(Debug)]
struct Ring {
    // TODO: this could be a resource version instead, and it wouldn't need to be computed.
    endpoint_group_hash: u64,
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

        self.endpoint_group_hash = endpoint_group.hash;
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

#[cfg(test)]
mod test_ring_hash {
    use super::*;

    #[test]
    fn test_rebuild_ring() {
        let mut ring = Ring {
            endpoint_group_hash: 0,
            entries: Vec::new(),
        };

        // rebuild a ring with no min size
        ring.rebuild(
            0,
            &EndpointGroup {
                hash: 111,
                endpoints: [(
                    Locality::Unknown,
                    vec![
                        EndpointAddress::SocketAddr("1.1.1.1:80".parse().unwrap()),
                        EndpointAddress::SocketAddr("1.1.1.2:80".parse().unwrap()),
                    ],
                )]
                .into(),
            },
        );

        assert_eq!(ring.endpoint_group_hash, 111);
        assert_eq!(ring.entries.len(), 2);
        assert_eq!(ring_indexes(&ring), (0..2).collect::<Vec<_>>());
        assert_hashes_unique(&ring);

        let first_ring = ring.entries.clone();

        // ignore a rebuild with the same hash
        ring.rebuild(
            0,
            &EndpointGroup {
                hash: 222,
                endpoints: [(
                    Locality::Unknown,
                    vec![
                        EndpointAddress::SocketAddr("1.1.1.1:80".parse().unwrap()),
                        EndpointAddress::SocketAddr("1.1.1.2:80".parse().unwrap()),
                        EndpointAddress::SocketAddr("1.1.1.3:80".parse().unwrap()),
                    ],
                )]
                .into(),
            },
        );

        assert_eq!(ring.endpoint_group_hash, 222);
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
            endpoint_group_hash: 0,
            entries: Vec::new(),
        };

        // rebuild a ring with no min size
        ring.rebuild(
            1024,
            &EndpointGroup {
                hash: 111,
                endpoints: [(
                    Locality::Unknown,
                    vec![
                        EndpointAddress::SocketAddr("1.1.1.1:80".parse().unwrap()),
                        EndpointAddress::SocketAddr("1.1.1.2:80".parse().unwrap()),
                        EndpointAddress::SocketAddr("1.1.1.3:80".parse().unwrap()),
                    ],
                )]
                .into(),
            },
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
            endpoint_group_hash: 0,
            entries: vec![],
        };
        ring.rebuild(
            0,
            &EndpointGroup {
                hash: 222,
                endpoints: [(
                    Locality::Unknown,
                    vec![
                        EndpointAddress::SocketAddr("1.1.1.1:80".parse().unwrap()),
                        EndpointAddress::SocketAddr("1.1.1.2:80".parse().unwrap()),
                        EndpointAddress::SocketAddr("1.1.1.3:80".parse().unwrap()),
                    ],
                )]
                .into(),
            },
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
    hash_params: &Vec<SessionAffinityHashParam>,
    url: &crate::Url,
    headers: &http::HeaderMap,
) -> Option<u64> {
    let mut hash: Option<u64> = None;

    for hash_param in hash_params {
        if let Some(new_hash) = hash_target(hash_param, url, headers) {
            hash = Some(match hash {
                Some(hash) => hash.rotate_left(1) ^ new_hash,
                None => new_hash,
            });

            if hash_param.terminal {
                break;
            }
        }
    }

    hash
}

fn hash_target(
    hash_param: &SessionAffinityHashParam,
    _url: &crate::Url,
    headers: &http::HeaderMap,
) -> Option<u64> {
    match &hash_param.matcher {
        SessionAffinityHashParamType::Header { name } => {
            let mut header_values: Vec<_> = headers
                .get_all(name)
                .into_iter()
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
    }
}

mod thread_local_xxhash {
    use std::cell::RefCell;
    use xxhash_rust::xxh64::Xxh64;

    // gRPC and Envoy use a zero seed. gRPC does this by definition, Envoy by
    // default.
    //
    // https://github.com/grpc/proposal/blob/master/A42-xds-ring-hash-lb-policy.md#xdsconfigselector-changes
    // https://github.com/envoyproxy/envoy/blob/main/source/common/common/hash.h#L22-L30
    // https://github.com/envoyproxy/envoy/blob/main/source/common/http/hash_policy.cc#L69
    const SEED: u64 = 0;

    thread_local! {
        static HASHER: RefCell<Xxh64> = const { RefCell::new(Xxh64::new(SEED)) };
    }

    /// Hash a single item using a thread-local [xx64 Hasher][Xxh64].
    pub(crate) fn hash<H: std::hash::Hash>(h: &H) -> u64 {
        HASHER.with_borrow_mut(|hasher| {
            hasher.reset(SEED);
            h.hash(hasher);
            hasher.digest()
        })
    }

    /// Hash an iterable of hashable items using a thread-local
    /// [xx64 Hasher][Xxh64].
    ///
    /// *Note*: Tuples implement [std::hash::Hash], so if you need to hash a
    /// sequence of items of different types, try passing a tuple to [hash].
    pub(crate) fn hash_iter<I: IntoIterator<Item = H>, H: std::hash::Hash>(iter: I) -> u64 {
        HASHER.with_borrow_mut(|hasher| {
            hasher.reset(SEED);
            for h in iter {
                h.hash(hasher)
            }
            hasher.digest()
        })
    }
}
