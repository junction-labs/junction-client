//! Controlled randomness.
//!
//! This module implements PRNGs that can all be deterministically seeded from a
//! single env variable, falling back to system entropy when ncessary. Using only
//! PRNG values seeded from this package allows using a seed to do deterministic
//! testing.
//!
//! This module is currently implemented using a two-stage strategy. A single
//! global PRNG wrapped in a `Mutex`` is used to lazily initialize thread local
//! PRNGs as necessary. After initialization, each thread has unfettered access
//! to a PRNG a-la `rand::thread_rng()`.

// NOTE: rand::ThreadRng is a thin wrapper on top of an Rc<UnsafeCell<_>> and
// liberally uses unsafe to take momentary access to a thread-local rng. We
// could do the same if the performance of this strategy isn't good enough.
//
//As of writing, we're not touching randomess nearly enough to have to think
//about that.

use std::cell::RefCell;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Call a function with access to a thread-local PRNG.
///
/// Prefer this to `rand::thread_rng()` from the `rand` crate - this fn is
/// seeded globally to enable deterministic testing, unlike the `rand`
/// crate's default thread-local Rng.
pub fn with_thread_rng<F, T>(f: F) -> T
where
    F: FnMut(&mut StdRng) -> T,
{
    thread_local! {
        static THREAD_RNG: RefCell<StdRng> = RefCell::new(seeded_std_rng());
    }

    THREAD_RNG.with_borrow_mut(f)
}

pub fn random<T>() -> T
where
    rand::distributions::Standard: rand::distributions::Distribution<T>,
{
    with_thread_rng(|rng| rng.gen())
}

fn seeded_std_rng() -> StdRng {
    let seed = {
        let mut rng = SEED_RNG.lock().unwrap();
        rng.gen()
    };
    StdRng::from_seed(seed)
}

static SEED_RNG: Lazy<Mutex<StdRng>> = Lazy::new(|| {
    let env_seed: Option<u64> = {
        match std::env::var("JUNCTION_SEED") {
            Ok(seed_str) => seed_str.parse().ok(),
            _ => None,
        }
    };

    let rng = match env_seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_entropy(),
    };

    Mutex::new(rng)
});
