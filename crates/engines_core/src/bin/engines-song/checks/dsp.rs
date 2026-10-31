//! The numpy / scipy pieces the render checks are built from, ported so the numbers match
//! the Python scripts they replace to float precision: `np.hanning`, `np.fft.rfft` magnitudes and
//! `rfftfreq`, `np.convolve` with a box, `np.percentile`, `scipy.signal.butter` as second-order
//! sections with `sosfilt`, and `find_peaks` (height, distance, prominence).

use std::f64::consts::PI;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// `np.hanning(n)`: the symmetric Hann window.
pub fn hanning(n: usize) -> Vec<f64> {
    match n {
        0 => Vec::new(),
        1 => vec![1.0],
        _ => (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos()).collect(),
    }
}

/// `np.fft.rfftfreq(n, 1 / sr)`, computed in numpy's order so a bin that sits on a band edge
/// falls on the same side of it.
pub fn rfftfreq(n: usize, sr: f64) -> Vec<f64> {
    let val = 1.0 / (n as f64 * (1.0 / sr));
    (0..=n / 2).map(|k| k as f64 * val).collect()
}

/// A planned real FFT of one length, reused across frames.
pub struct Rfft {
    plan: Arc<dyn RealToComplex<f64>>,
    input: Vec<f64>,
    output: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
}

impl Rfft {
    pub fn new(n: usize) -> Self {
        let plan = RealFftPlanner::<f64>::new().plan_fft_forward(n);
        let (input, output, scratch) = (plan.make_input_vec(), plan.make_output_vec(), plan.make_scratch_vec());
        Rfft { plan, input, output, scratch }
    }

    /// `|rfft(x · w)|` into `out` (`n / 2 + 1` bins). `x` and `w` are `n` long.
    pub fn magnitudes(&mut self, x: &[f64], w: &[f64], out: &mut [f64]) {
        for ((i, &a), &b) in self.input.iter_mut().zip(x).zip(w) {
            *i = a * b;
        }
        self.plan.process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch).expect("rfft lengths are planned");
        for (o, c) in out.iter_mut().zip(&self.output) {
            *o = c.norm();
        }
    }
}

/// `|rfft(x · hanning(len(x)))|²`: one windowed power spectrum of the whole of `x`.
pub fn power_spectrum(x: &[f64]) -> Vec<f64> {
    if x.is_empty() {
        return Vec::new();
    }
    let mut f = Rfft::new(x.len());
    let mut mag = vec![0.0; x.len() / 2 + 1];
    f.magnitudes(x, &hanning(x.len()), &mut mag);
    mag.iter().map(|m| m * m).collect()
}

/// `np.convolve(a, np.ones(k), mode)`'s two shapes the checks use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Conv {
    /// Length `max(len, k)`, centred as numpy centres it.
    Same,
    /// Only where the box fits: `max(len, k) − min(len, k) + 1`.
    Valid,
}

/// `np.convolve(a, np.ones(k) * scale, mode)`: a moving sum over `k` samples times `scale` (`1 / k`
/// for a moving mean). Prefix sums, clamped at 0 when every input is non-negative, so a window of
/// near-silence can't round below zero before a square root.
pub fn box_conv(a: &[f64], k: usize, scale: f64, mode: Conv) -> Vec<f64> {
    let (n, k) = (a.len(), k.max(1));
    if n == 0 {
        return Vec::new();
    }
    let non_negative = a.iter().all(|&v| v >= 0.0);
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0f64);
    for &v in a {
        prefix.push(prefix.last().copied().unwrap_or(0.0) + v);
    }
    // full[j] = Σ a[i] for max(0, j − k + 1) ≤ i ≤ min(j, n − 1), j in 0 .. n + k − 1.
    let full = |j: usize| -> f64 {
        let lo = (j + 1).saturating_sub(k);
        let hi = (j + 1).min(n);
        let s = (prefix[hi] - prefix[lo]) * scale;
        if non_negative {
            s.max(0.0)
        } else {
            s
        }
    };
    let (long, short) = (n.max(k), n.min(k));
    let (start, len) = match mode {
        Conv::Same => ((short - 1) / 2, long),
        Conv::Valid => (short - 1, long - short + 1),
    };
    (start..start + len).map(full).collect()
}

/// `np.mean`.
pub fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}

/// `np.std` (population, `ddof = 0`).
pub fn std(v: &[f64]) -> f64 {
    let m = mean(v);
    (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

/// `np.percentile(v, q)` with numpy's default linear interpolation (`NaN` for an empty slice).
pub fn percentile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let idx = q / 100.0 * (s.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = (lo + 1).min(s.len() - 1);
    let t = idx - lo as f64;
    let (a, b) = (s[lo], s[hi]);
    // numpy's `_lerp`: from the nearer end, so t = 1 lands exactly on b.
    if t >= 0.5 {
        b - (b - a) * (1.0 - t)
    } else {
        a + (b - a) * t
    }
}

/// `np.median`.
pub fn median(v: &[f64]) -> f64 {
    percentile(v, 50.0)
}

/// `20·log10(max(v, 1e-12))`, the scripts' dB.
pub fn db(v: f64) -> f64 {
    20.0 * v.max(1e-12).log10()
}

/// A Butterworth response for [`butter`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Band {
    /// Highpass at this edge (Hz).
    High(f64),
    /// Bandpass between these edges (Hz).
    Pass(f64, f64),
}

/// One biquad `[b0, b1, b2, a1, a2]` (`a0` = 1).
type Section = [f64; 5];

fn c(re: f64) -> Complex<f64> {
    Complex::new(re, 0.0)
}

fn prod(v: &[Complex<f64>]) -> Complex<f64> {
    v.iter().fold(c(1.0), |acc, x| acc * x)
}

/// `scipy.signal.butter(order, edges, btype, fs=sr, output="sos")`: scipy's own path — the analog
/// prototype, `lp2hp_zpk` / `lp2bp_zpk` on pre-warped edges, `bilinear_zpk` — grouped into
/// biquads by conjugate pole pairs. The sections pair poles and zeros differently from scipy's
/// `zpk2sos`, which changes nothing but rounding: the cascade is the same filter.
pub fn butter(order: usize, band: Band, sr: f64) -> Vec<Section> {
    assert!(order > 0 && order % 2 == 0, "even orders only (the checks use 4)");
    // buttap: poles on the left half of the unit circle, no zeros, gain 1.
    let proto: Vec<Complex<f64>> = (0..order)
        .map(|i| {
            let m = -(order as f64) + 1.0 + 2.0 * i as f64;
            -Complex::from_polar(1.0, PI * m / (2.0 * order as f64))
        })
        .collect();
    // iirfilter: normalise to Nyquist, then pre-warp for fs = 2.
    let warp = |hz: f64| 4.0 * (PI * (hz / (sr / 2.0)) / 2.0).tan();
    let (zeros_at_dc, poles, k) = match band {
        Band::High(hz) => {
            let wo = warp(hz);
            let p: Vec<Complex<f64>> = proto.iter().map(|p| c(wo) / p).collect();
            let neg: Vec<Complex<f64>> = proto.iter().map(|p| -p).collect();
            (order, p, (c(1.0) / prod(&neg)).re)
        }
        Band::Pass(lo, hi) => {
            let (w1, w2) = (warp(lo), warp(hi));
            let (bw, wo) = (w2 - w1, (w1 * w2).sqrt());
            let mut p = Vec::with_capacity(2 * order);
            for q in &proto {
                let pl = q * (bw / 2.0);
                let r = (pl * pl - c(wo * wo)).sqrt();
                p.push(pl + r);
                p.push(pl - r);
            }
            (order, p, bw.powi(order as i32))
        }
    };
    // bilinear_zpk at fs = 2 (fs2 = 4): s-plane zeros at 0 land on z = 1; the degree's worth of
    // zeros at infinity land on z = −1.
    let fs2 = 4.0;
    let pz: Vec<Complex<f64>> = poles.iter().map(|p| (c(fs2) + p) / (c(fs2) - p)).collect();
    let den: Vec<Complex<f64>> = poles.iter().map(|p| c(fs2) - p).collect();
    let k = k * (c(fs2.powi(zeros_at_dc as i32)) / prod(&den)).re;
    let at_minus_one = poles.len() - zeros_at_dc;
    // One biquad per conjugate pair; each takes two zeros (+1 +1, +1 −1, or −1 −1).
    let mut upper: Vec<Complex<f64>> = pz.into_iter().filter(|p| p.im > 0.0).collect();
    upper.sort_by(|a, b| a.re.total_cmp(&b.re));
    let (mut plus, mut minus) = (zeros_at_dc, at_minus_one);
    upper
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let take = |n: &mut usize| if *n > 0 { *n -= 1; true } else { false };
            let z1 = if take(&mut plus) { 1.0 } else if take(&mut minus) { -1.0 } else { 0.0 };
            let z2 = if take(&mut minus) { -1.0 } else if take(&mut plus) { 1.0 } else { 0.0 };
            let g = if i == 0 { k } else { 1.0 };
            [g, -g * (z1 + z2), g * z1 * z2, -2.0 * p.re, p.norm_sqr()]
        })
        .collect()
}

/// `scipy.signal.sosfilt` as a stream (direct form II transposed, zero initial state).
#[derive(Clone, Debug)]
pub struct Sos {
    sections: Vec<Section>,
    state: Vec<[f64; 2]>,
}

impl Sos {
    pub fn new(sections: Vec<Section>) -> Self {
        let state = vec![[0.0; 2]; sections.len()];
        Sos { sections, state }
    }

    pub fn butter(order: usize, band: Band, sr: f64) -> Self {
        Self::new(butter(order, band, sr))
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let mut v = x;
        for (s, z) in self.sections.iter().zip(&mut self.state) {
            let y = s[0] * v + z[0];
            z[0] = s[1] * v - s[3] * y + z[1];
            z[1] = s[2] * v - s[4] * y;
            v = y;
        }
        v
    }

    /// Filter a whole signal from rest.
    pub fn filter(mut self, x: &[f64]) -> Vec<f64> {
        x.iter().map(|&v| self.process(v)).collect()
    }
}

/// `find_peaks`' filters, applied in scipy's order: height, distance, prominence.
#[derive(Clone, Copy, Debug, Default)]
pub struct Peaks {
    pub height: Option<f64>,
    pub distance: Option<f64>,
    pub prominence: Option<f64>,
}

/// `scipy.signal.find_peaks(x, …)`: local maxima (a flat top counts once, at its middle) kept by
/// the filters in `o`.
pub fn find_peaks(x: &[f64], o: Peaks) -> Vec<usize> {
    let n = x.len();
    let mut peaks = Vec::new();
    let mut i = 1;
    while i + 1 < n {
        if x[i - 1] < x[i] {
            let mut ahead = i + 1;
            while ahead + 1 < n && x[ahead] == x[i] {
                ahead += 1;
            }
            if x[ahead] < x[i] {
                peaks.push((i + ahead - 1) / 2);
                i = ahead;
            }
        }
        i += 1;
    }
    if let Some(h) = o.height {
        peaks.retain(|&p| x[p] >= h);
    }
    if let Some(d) = o.distance {
        peaks = by_distance(x, &peaks, d.ceil() as usize);
    }
    if let Some(pmin) = o.prominence {
        peaks.retain(|&p| prominence(x, p) >= pmin);
    }
    peaks
}

/// scipy's `_select_by_peak_distance`: the highest peaks first, each clearing its neighbours closer
/// than `distance`.
fn by_distance(x: &[f64], peaks: &[usize], distance: usize) -> Vec<usize> {
    let mut keep = vec![true; peaks.len()];
    let mut order: Vec<usize> = (0..peaks.len()).collect();
    order.sort_by(|&a, &b| x[peaks[a]].total_cmp(&x[peaks[b]]));
    for &j in order.iter().rev() {
        if !keep[j] {
            continue;
        }
        let mut k = j;
        while k > 0 && peaks[j] - peaks[k - 1] < distance {
            keep[k - 1] = false;
            k -= 1;
        }
        let mut k = j + 1;
        while k < peaks.len() && peaks[k] - peaks[j] < distance {
            keep[k] = false;
            k += 1;
        }
    }
    peaks.iter().zip(keep).filter(|(_, k)| *k).map(|(&p, _)| p).collect()
}

/// scipy's `_peak_prominences` with no window: how far the peak stands above the higher of the
/// lowest points between it and the nearest higher sample on each side.
fn prominence(x: &[f64], peak: usize) -> f64 {
    let top = x[peak];
    let mut left_min = top;
    let mut i = peak as isize;
    while i >= 0 && x[i as usize] <= top {
        left_min = left_min.min(x[i as usize]);
        i -= 1;
    }
    let mut right_min = top;
    let mut i = peak;
    while i < x.len() && x[i] <= top {
        right_min = right_min.min(x[i]);
        i += 1;
    }
    top - left_min.max(right_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f64], b: &[f64], tol: f64) {
        assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() <= tol * y.abs().max(1.0), "{a:?} vs {b:?}");
        }
    }

    /// numpy's own outputs for the same inputs.
    #[test]
    fn windows_and_box_convolutions_match_numpy() {
        close(&hanning(5), &[0.0, 0.5, 1.0, 0.5, 0.0], 1e-15);
        let a: Vec<f64> = (0..10).map(f64::from).collect();
        close(&box_conv(&a, 4, 1.0, Conv::Same), &[1., 3., 6., 10., 14., 18., 22., 26., 30., 24.], 1e-12);
        close(&box_conv(&a, 3, 1.0, Conv::Same), &[1., 3., 6., 9., 12., 15., 18., 21., 24., 17.], 1e-12);
        close(&box_conv(&a, 4, 1.0, Conv::Valid), &[6., 10., 14., 18., 22., 26., 30.], 1e-12);
        // A box longer than the signal: numpy swaps them.
        close(&box_conv(&[0., 1., 2.], 5, 1.0, Conv::Same), &[1., 3., 3., 3., 3.], 1e-12);
        assert!((percentile(&[1., 2., 3., 4., 10.], 97.0) - 9.28).abs() < 1e-12);
        assert!((percentile(&[1., 2., 3., 4., 10.], 99.9) - 9.976).abs() < 1e-12);
        assert_eq!(median(&[1., 3., 2., 4.]), 2.5);
        assert_eq!(rfftfreq(8, 8.0), vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    /// The cascade's response equals scipy's sections' at a few frequencies (`sosfreqz`).
    #[test]
    fn butterworth_sections_are_scipys_filter() {
        let response = |s: &[Section], hz: f64, sr: f64| {
            let z = Complex::from_polar(1.0, 2.0 * PI * hz / sr);
            s.iter().fold(c(1.0), |acc, q| {
                let zi = c(1.0) / z;
                acc * (c(q[0]) + zi * q[1] + zi * zi * q[2]) / (c(1.0) + zi * q[3] + zi * zi * q[4])
            })
        };
        // scipy: butter(4, 1000/24000, 'high', output='sos') and butter(4, [88, 396], 'band', fs=48000).
        let scipy_hp: [Section; 2] = [
            [0.84267663, -1.68535325, 0.84267663, -1.76950435, 0.78477333],
            [1.0, -2.0, 1.0, -1.88855595, 0.90485223],
        ];
        let scipy_bp: [Section; 4] = [
            [1.56744989e-07, 3.13489978e-07, 1.56744989e-07, -1.94769777, 0.949065957],
            [1.0, 2.0, 1.0, -1.97284646, 0.975346633],
            [1.0, -2.0, 1.0, -1.97776058, 0.978011755],
            [1.0, -2.0, 1.0, -1.99398778, 0.994128192],
        ];
        let ours_hp = butter(4, Band::High(1000.0), 48_000.0);
        let ours_bp = butter(4, Band::Pass(88.0, 396.0), 48_000.0);
        for hz in [100.0, 500.0, 1000.0, 2000.0, 8000.0] {
            let (a, b) = (response(&ours_hp, hz, 48_000.0).norm(), response(&scipy_hp, hz, 48_000.0).norm());
            assert!((a - b).abs() < 1e-6, "hp at {hz}: {a} vs {b}");
        }
        for hz in [40.0, 88.0, 190.0, 396.0, 1000.0] {
            let (a, b) = (response(&ours_bp, hz, 48_000.0).norm(), response(&scipy_bp, hz, 48_000.0).norm());
            // scipy's coefficients above are printed to 9 digits: 1e-4 is their rounding.
            assert!((a - b).abs() < 1e-4 * b.max(1e-3), "bp at {hz}: {a} vs {b}");
        }
        // And it filters: DC through the highpass dies away.
        let y = Sos::butter(4, Band::High(1000.0), 48_000.0).filter(&vec![1.0; 48_000]);
        assert!(y[47_999].abs() < 1e-9);
    }

    #[test]
    fn peaks_follow_scipys_rules() {
        // A flat top counts once, at its middle; the ends never count.
        assert_eq!(find_peaks(&[3., 1., 2., 2., 2., 1., 4.], Peaks::default()), vec![3]);
        let x = [0., 5., 0., 1., 0., 4., 0., 6., 0.];
        assert_eq!(find_peaks(&x, Peaks::default()), vec![1, 3, 5, 7]);
        assert_eq!(find_peaks(&x, Peaks { height: Some(4.0), ..Default::default() }), vec![1, 5, 7]);
        // Distance, highest first: the 6 clears the 4 two samples away, then the 5 clears the 1.
        assert_eq!(find_peaks(&x, Peaks { distance: Some(3.0), ..Default::default() }), vec![1, 7]);
        // Prominence: the 1 stands 1 above its valleys, the 4 stands 4.
        assert_eq!(find_peaks(&x, Peaks { prominence: Some(2.0), ..Default::default() }), vec![1, 5, 7]);
    }
}
