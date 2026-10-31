//! Portable, fixed deterministic hashing (CE-R7.1).
//!
//! Every probabilistic site derives an independent stream from
//! `(loop_id, source_id, step, voice_id)`. This hash is **fixed and documented**
//! so it is byte-identical across macOS/iOS and any offline re-render.
//! **Do not** substitute a language/stdlib hasher (their string hashing is
//! implementation-defined — the M2 C++ `std::hash` portability bug, see ADR 0003).

/// FNV-1a over bytes → u64. Used to fold string ids (`source_id`, `voice_id`) into
/// a stable integer before mixing.
pub fn fnv1a_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325; // FNV offset basis
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x00000100000001B3); // FNV prime
    }
    h
}

/// Four-input mix: FNV-1a accumulation + murmur3-style finalizer per input.
/// Decorrelates adjacent `loop_id`s and is order-independent of evaluation.
pub fn hash64(a: u64, b: u64, c: u64, d: u64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(0x00000100000001B3);
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51afd7ed558ccd);
        h ^= h >> 33;
    };
    mix(a);
    mix(b);
    mix(c);
    mix(d);
    h
}

/// Convenience for the canonical probabilistic site key (CE-R7.1).
pub fn hash_site(loop_id: u64, source_id: &str, step: u64, voice_id: &str) -> u64 {
    hash64(
        loop_id,
        fnv1a_bytes(source_id.as_bytes()),
        step,
        fnv1a_bytes(voice_id.as_bytes()),
    )
}

/// Map a hash to a float in `[0, 1)`.
pub fn hash_to_unit(h: u64) -> f32 {
    // Top 24 bits → [0,1) at f32 precision.
    ((h >> 40) as f32) * (1.0 / ((1u64 << 24) as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        let a = hash_site(413, "spark_events", 2, "e");
        let b = hash_site(413, "spark_events", 2, "e");
        assert_eq!(a, b, "same site must hash identically");
        let u = hash_to_unit(a);
        assert!((0.0..1.0).contains(&u), "unit out of range: {u}");
    }

    #[test]
    fn distinct_sites_differ() {
        let base = hash_site(413, "chords", 8, "v4");
        assert_ne!(base, hash_site(414, "chords", 8, "v4")); // loop_id
        assert_ne!(base, hash_site(413, "bass_line", 8, "v4")); // source
        assert_ne!(base, hash_site(413, "chords", 9, "v4")); // step
        assert_ne!(base, hash_site(413, "chords", 8, "v1")); // voice
    }

    #[test]
    fn adjacent_loop_ids_decorrelate() {
        // Naive adjacent seeds produce visibly similar early draws; the mix should not.
        let u0 = hash_to_unit(hash_site(1000, "lfo", 0, ""));
        let u1 = hash_to_unit(hash_site(1001, "lfo", 0, ""));
        assert!((u0 - u1).abs() > 0.01, "adjacent loop_ids too correlated: {u0} {u1}");
    }
}
