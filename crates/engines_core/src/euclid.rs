//! Euclidean rhythm generator (CE-R5.3, `generator: {kind: "euclid", pulses, rotation}`).
//!
//! Bjorklund / rotating-remainders: distribute `pulses` ones across `steps` as evenly
//! as possible, then rotate left by `rotation`. Ported from the M2 C++ implementation.

/// Returns a binary vector of length `steps` with `pulses` ones distributed evenly,
/// rotated left by `rotation`.
pub fn euclidean_rhythm(pulses: u32, steps: u32, rotation: i64) -> Vec<u8> {
    if steps == 0 {
        return Vec::new();
    }
    let steps = steps as usize;
    let mut pulses = (pulses as usize).min(steps);

    // Seed groups: `pulses` ones then the remainder zeros.
    let mut groups: Vec<Vec<u8>> = Vec::with_capacity(steps);
    for _ in 0..pulses {
        groups.push(vec![1]);
    }
    for _ in 0..(steps - pulses) {
        groups.push(vec![0]);
    }

    loop {
        let rem = groups.len() - pulses;
        if pulses <= 1 || rem <= 1 {
            break;
        }
        let pairs = pulses.min(rem);
        for i in 0..pairs {
            let tail = groups[pulses + i].clone();
            groups[i].extend(tail);
        }
        groups.drain(pulses..pulses + pairs);
        pulses = pairs;
    }

    let mut out: Vec<u8> = groups.into_iter().flatten().collect();

    if rotation != 0 && !out.is_empty() {
        let n = out.len() as i64;
        let rot = (((rotation % n) + n) % n) as usize;
        out.rotate_left(rot);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_and_pulse_count() {
        let r = euclidean_rhythm(17, 44, 0);
        assert_eq!(r.len(), 44);
        assert_eq!(r.iter().map(|&b| b as u32).sum::<u32>(), 17);
    }

    #[test]
    fn edge_cases() {
        assert_eq!(euclidean_rhythm(0, 4, 0), vec![0, 0, 0, 0]);
        assert_eq!(euclidean_rhythm(4, 4, 0), vec![1, 1, 1, 1]);
        assert_eq!(euclidean_rhythm(9, 4, 0).iter().sum::<u8>(), 4); // clamp pulses<=steps
        assert!(euclidean_rhythm(5, 0, 0).is_empty());
    }

    #[test]
    fn rotation_preserves_count_and_shifts() {
        let base = euclidean_rhythm(9, 23, 0);
        let rot = euclidean_rhythm(9, 23, 4);
        assert_eq!(rot.len(), base.len());
        assert_eq!(
            base.iter().map(|&b| b as u32).sum::<u32>(),
            rot.iter().map(|&b| b as u32).sum::<u32>()
        );
        // rotation by n is identity
        assert_eq!(base, euclidean_rhythm(9, 23, 23));
    }

    #[test]
    fn classic_e3_8_is_evenly_spread() {
        // E(3,8): three pulses, evenly spread (one per ~2.67 steps).
        let r = euclidean_rhythm(3, 8, 0);
        assert_eq!(r.len(), 8);
        assert_eq!(r.iter().map(|&b| b as u32).sum::<u32>(), 3);
        assert_eq!(r[0], 1, "first step is a pulse");
    }
}
