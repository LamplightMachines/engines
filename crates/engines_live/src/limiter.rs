//! The live-only protection limiter (ADR 0031/0036): sits on `engines-live`'s device output so a
//! mid-edit typo (a level of 8, a feedback of 0.99) can't blast the speakers. **Never** in renders
//! or stems — the master there stays `mix · master_gain`.
//!
//! A stereo-linked lookahead peak limiter, correct by construction:
//! 1. each frame's target gain is `min(1, ceiling / peak)`;
//! 2. a sliding **minimum** over the last `L + 1` targets (`L` = lookahead frames);
//! 3. a **box average** over the last `L + 1` minima — a smooth ramp into the peak;
//! 4. the gain may fall instantly to that value but recovers only at the release rate;
//! 5. the signal is delayed by `L` frames before the gain is applied.
//!
//! Every minimum averaged for the frame now leaving the delay line includes that frame's own
//! target, so the applied gain never exceeds it: the ceiling holds without clipping (a final clamp
//! is a backstop against float rounding only). Below the ceiling every target is exactly 1, the
//! averaged gain is exactly 1, and the output is the input delayed by `L` frames, bit for bit.
//! Non-finite samples are muted (and counted). RT-safe: no allocation after construction.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// What the audio thread reports to the UI: the deepest gain since the last read, NaNs muted.
#[derive(Debug, Default)]
pub struct LimiterMeter {
    min_gain_bits: AtomicU32,
    nan_muted: AtomicU64,
}

impl LimiterMeter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { min_gain_bits: AtomicU32::new(1.0f32.to_bits()), nan_muted: AtomicU64::new(0) })
    }
    /// The deepest gain reduction since the last call, in dB (≤ 0), and resets it.
    pub fn take_gr_db(&self) -> f32 {
        let g = f32::from_bits(self.min_gain_bits.swap(1.0f32.to_bits(), Ordering::Relaxed));
        20.0 * g.max(1e-6).log10()
    }
    /// Non-finite samples muted so far.
    pub fn nan_muted(&self) -> u64 {
        self.nan_muted.load(Ordering::Relaxed)
    }
    fn note_gain(&self, g: f32) {
        let cur = f32::from_bits(self.min_gain_bits.load(Ordering::Relaxed));
        if g < cur {
            self.min_gain_bits.store(g.to_bits(), Ordering::Relaxed);
        }
    }
}

pub struct Limiter {
    ceiling: f32,
    channels: usize,
    look: usize,
    delay: Vec<f32>,
    delay_pos: usize,
    // Monotonic deque (fixed ring) for the sliding minimum: (frame index, target).
    dq_idx: Vec<u64>,
    dq_val: Vec<f32>,
    dq_head: usize,
    dq_len: usize,
    // Box average over the last `look + 1` minima.
    box_buf: Vec<f32>,
    box_pos: usize,
    box_sum: f64,
    n: u64,
    gain: f32,
    release_step: f32,
    meter: Arc<LimiterMeter>,
    /// Samples the final clamp had to cut by more than float rounding — always 0 when the
    /// sliding minimum is right (the tests hold it there).
    backstop: u64,
}

impl Limiter {
    /// `ceiling_dbfs` (e.g. −1), `lookahead_ms` (e.g. 1.5), `release_ms` (full recovery time).
    pub fn new(sample_rate: f64, channels: usize, ceiling_dbfs: f32, lookahead_ms: f64, release_ms: f64, meter: Arc<LimiterMeter>) -> Self {
        let look = ((sample_rate * lookahead_ms / 1000.0).round() as usize).max(1);
        let w = look + 1;
        Self {
            ceiling: 10f32.powf(ceiling_dbfs / 20.0),
            channels: channels.max(1),
            look,
            delay: vec![0.0; look * channels.max(1)],
            delay_pos: 0,
            dq_idx: vec![0; w],
            dq_val: vec![1.0; w],
            dq_head: 0,
            dq_len: 0,
            box_buf: vec![1.0; w],
            box_pos: 0,
            box_sum: w as f64,
            n: 0,
            gain: 1.0,
            release_step: (1.0 / (sample_rate * release_ms / 1000.0).max(1.0)) as f32,
            meter,
            backstop: 0,
        }
    }

    /// Latency in frames.
    pub fn lookahead(&self) -> usize {
        self.look
    }

    /// Process interleaved frames in place.
    pub fn process(&mut self, data: &mut [f32]) {
        let ch = self.channels;
        let w = self.look + 1;
        let mut nan = 0u64;
        let mut deepest = 1.0f32;
        for frame in data.chunks_exact_mut(ch) {
            let mut peak = 0.0f32;
            let (mut sum_l, mut sum_r) = (0.0f32, 0.0f32);
            for (c, s) in frame.iter_mut().enumerate() {
                if !s.is_finite() {
                    *s = 0.0;
                    nan += 1;
                }
                peak = peak.max(s.abs());
                if c % 2 == 0 {
                    sum_l += *s;
                } else {
                    sum_r += *s;
                }
            }
            // Routed pairs (ADR 0044) are summed downstream (the DAW's Main): hold that sum — every
            // even channel and every odd one — under the ceiling too, with the same linked gain, so
            // the pairs still add up to the master. Stereo: the sums are the channels themselves.
            if ch > 2 {
                peak = peak.max(sum_l.abs()).max(sum_r.abs());
            }
            let target = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };

            // Sliding minimum over frames n-L..n. Expire what left the window first: the ring
            // holds L + 1 entries, and a rising run of targets (a hit dying away) fills it.
            while self.dq_len > 0 && self.dq_idx[self.dq_head] + (self.look as u64) < self.n {
                self.dq_head = (self.dq_head + 1) % w;
                self.dq_len -= 1;
            }
            while self.dq_len > 0 {
                let back = (self.dq_head + self.dq_len - 1) % w;
                if self.dq_val[back] >= target {
                    self.dq_len -= 1;
                } else {
                    break;
                }
            }
            let slot = (self.dq_head + self.dq_len) % w;
            self.dq_idx[slot] = self.n;
            self.dq_val[slot] = target;
            self.dq_len += 1;
            let wmin = self.dq_val[self.dq_head];

            // Box average of the minima ⇒ a smooth ramp that still never exceeds the target.
            self.box_sum += wmin as f64 - self.box_buf[self.box_pos] as f64;
            self.box_buf[self.box_pos] = wmin;
            self.box_pos = (self.box_pos + 1) % w;
            let avg = (self.box_sum / w as f64) as f32;
            self.gain = avg.min(self.gain + self.release_step).min(1.0);
            deepest = deepest.min(self.gain);

            // Delay by L frames, then apply.
            let base = self.delay_pos * ch;
            for (c, s) in frame.iter_mut().enumerate() {
                let delayed = self.delay[base + c];
                self.delay[base + c] = *s;
                let y = delayed * self.gain;
                if y.abs() > self.ceiling * 1.0001 {
                    self.backstop += 1;
                }
                *s = y.clamp(-self.ceiling, self.ceiling); // backstop: float rounding only
            }
            self.delay_pos = (self.delay_pos + 1) % self.look;
            self.n += 1;
        }
        if nan > 0 {
            self.meter.nan_muted.fetch_add(nan, Ordering::Relaxed);
        }
        self.meter.note_gain(deepest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn limiter() -> (Limiter, Arc<LimiterMeter>) {
        let m = LimiterMeter::new();
        (Limiter::new(SR, 2, -1.0, 1.5, 100.0, m.clone()), m)
    }

    fn sine(n: usize, amp: f32, hz: f32) -> Vec<f32> {
        (0..n).flat_map(|i| {
            let v = amp * (2.0 * std::f32::consts::PI * hz * i as f32 / SR as f32).sin();
            [v, -v]
        })
        .collect()
    }

    #[test]
    fn below_the_ceiling_it_is_a_pure_delay_bit_for_bit() {
        let (mut l, m) = limiter();
        let input = sine(10_000, 0.8, 220.0);
        let mut out = input.clone();
        for chunk in out.chunks_mut(512) {
            l.process(chunk);
        }
        let d = l.lookahead() * 2;
        assert!(out[..d].iter().all(|&x| x == 0.0));
        assert_eq!(&out[d..], &input[..input.len() - d], "exactly the input, delayed");
        assert_eq!(m.take_gr_db(), 0.0);
    }

    #[test]
    fn the_ceiling_is_never_exceeded() {
        let (mut l, m) = limiter();
        let ceiling = 10f32.powf(-1.0 / 20.0);
        // A quiet passage, then a sudden 18 dB-over burst with sharp transients, then quiet again.
        let mut input = sine(4_000, 0.3, 110.0);
        input.extend(sine(4_000, 8.0, 3_000.0));
        input.extend((0..2_000).flat_map(|i| if i % 97 == 0 { [20.0, -20.0] } else { [0.1, 0.1] }));
        input.extend(sine(8_000, 0.3, 110.0));
        let mut out = input.clone();
        for chunk in out.chunks_mut(256) {
            l.process(chunk);
        }
        let peak = out.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        assert!(peak <= ceiling, "peak {peak} > ceiling {ceiling}");
        assert_eq!(l.backstop, 0, "held by the gain, not the clamp");
        assert!(peak > ceiling * 0.95, "it limits to the ceiling, not far below: {peak}");
        assert!(m.take_gr_db() < -20.0);
        // After the release time the quiet tail passes untouched again.
        let d = l.lookahead() * 2;
        let tail = out.len() - 2_000;
        assert_eq!(&out[tail..], &input[tail - d..input.len() - d]);
    }

    #[test]
    fn a_dying_hit_is_limited_by_the_gain_not_the_clamp() {
        // Peaks falling frame by frame (a loud hit decaying) make the targets rise: a run longer
        // than the window must not overfill the sliding-minimum ring and under-report the minimum.
        let (mut l, _) = limiter();
        let ceiling = 10f32.powf(-1.0 / 20.0);
        let mut input = vec![0.1f32; 2 * 500];
        input.extend((0..800).flat_map(|i| {
            let a = 6.0 - 4.8 * i as f32 / 800.0;
            [a, a]
        }));
        input.extend(vec![0.1f32; 2 * 500]);
        let mut out = input.clone();
        for chunk in out.chunks_mut(128) {
            l.process(chunk);
        }
        assert_eq!(l.backstop, 0, "the gain alone held the ceiling");
        assert!(out.iter().all(|x| x.abs() <= ceiling));
    }

    #[test]
    fn the_gain_ramps_into_a_peak_instead_of_stepping() {
        let (mut l, _) = limiter();
        let mut input = vec![0.5f32; 2 * 1_000];
        input.extend(vec![4.0f32; 2 * 1_000]);
        let mut out = input.clone();
        l.process(&mut out);
        // Before the loud block arrives (delayed), the gain eases down over ~L frames.
        let d = l.lookahead();
        let onset = 1_000 + d; // first loud frame at the output
        let before: Vec<f32> = (onset - d..onset).map(|f| out[2 * f] / 0.5).collect();
        assert!(before.windows(2).all(|w| w[1] <= w[0] + 1e-6), "monotone ramp down");
        assert!(before.first().unwrap() > &0.9 && before.last().unwrap() < &0.3, "{before:?}");
    }

    #[test]
    fn routed_pairs_are_held_under_the_ceiling_as_a_sum() {
        // Four 4-channel pairs each at 0.6 (under the −1 dBFS ceiling alone) sum to 2.4 in the DAW:
        // the linked gain holds that sum — and so every pair, by the same factor — under the ceiling.
        let m = LimiterMeter::new();
        let mut l = Limiter::new(SR, 8, -1.0, 1.5, 100.0, m.clone());
        let mut out: Vec<f32> = (0..20_000).flat_map(|_| [0.6f32, -0.6, 0.6, -0.6, 0.6, -0.6, 0.6, -0.6]).collect();
        for chunk in out.chunks_mut(8 * 256) {
            l.process(chunk);
        }
        let ceiling = 10f32.powf(-1.0 / 20.0);
        let settled = &out[out.len() - 8 * 1000..];
        for f in settled.chunks_exact(8) {
            let (sl, sr): (f32, f32) = (f.iter().step_by(2).sum(), f.iter().skip(1).step_by(2).sum());
            assert!(sl.abs() <= ceiling + 1e-6 && sr.abs() <= ceiling + 1e-6, "sums {sl} {sr}");
            assert!(f.iter().all(|x| (x.abs() - f[0].abs()).abs() < 1e-7), "one linked gain for every channel");
        }
        assert!(m.take_gr_db() < -8.0, "it reduced the gain by ~ 20·log10(0.891/2.4)");
    }

    #[test]
    fn non_finite_samples_are_muted_and_counted() {
        let (mut l, m) = limiter();
        let mut buf = vec![0.2f32; 2 * 200];
        buf[10] = f32::NAN;
        buf[11] = f32::INFINITY;
        l.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
        assert_eq!(m.nan_muted(), 2);
    }
}
