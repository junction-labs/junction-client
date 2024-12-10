/// Convenience functions for doing things with a thread-local Xxhash hasher.
pub(crate) mod thread_local_xxhash {
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
