//! Live spectrogram for the instrument screen (ADR 0024). Each pushed 25 Hz audio hop becomes one
//! column of `rows` normalized 0..1 log-frequency magnitudes (bottom row = low freq), kept in a
//! rolling ring of `cap` columns. FFT runs here in the TUI (the core only ships raw hops), reusing
//! the same `realfft` crate the engine's feature extractor uses.

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;
use std::collections::VecDeque;

const FFT_N: usize = 2048;
/// dB floor mapped to display 0; 0 dBFS maps to 1.
const DB_FLOOR: f32 = -70.0;
/// Lowest displayed frequency (Hz) — below this is sub-bass mud not worth a row.
const F_LO: f32 = 40.0;

/// A rolling log-frequency spectrogram.
pub struct Spectro {
    planner: RealFftPlanner<f32>,
    window: Vec<f32>,
    cols: VecDeque<Vec<f32>>,
    cap: usize,
    rows: usize,
    nyq: f32,
}

impl Spectro {
    pub fn new(rows: usize, cap: usize, sample_rate: f32) -> Self {
        // Hann window over the FFT frame.
        let window = (0..FFT_N)
            .map(|i| {
                let s = (std::f32::consts::PI * i as f32 / (FFT_N as f32 - 1.0)).sin();
                s * s
            })
            .collect();
        Self {
            planner: RealFftPlanner::<f32>::new(),
            window,
            cols: VecDeque::new(),
            cap: cap.max(1),
            rows: rows.max(1),
            nyq: (sample_rate * 0.5).max(F_LO * 2.0),
        }
    }

    pub fn cols(&self) -> &VecDeque<Vec<f32>> {
        &self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn clear(&mut self) {
        self.cols.clear();
    }

    /// FFT one mono audio hop and append its log-frequency magnitude column.
    pub fn push_hop(&mut self, hop: &[f32]) {
        if hop.is_empty() {
            return;
        }
        let r2c = self.planner.plan_fft_forward(FFT_N);
        let mut input = r2c.make_input_vec(); // zeroed, len FFT_N
        let take = hop.len().min(FFT_N);
        for i in 0..take {
            input[i] = hop[i] * self.window[i];
        }
        let mut spec = r2c.make_output_vec(); // len FFT_N/2 + 1
        if r2c.process(&mut input, &mut spec).is_err() {
            return;
        }
        let mag: Vec<f32> = spec.iter().map(|c: &Complex<f32>| c.norm()).collect();
        let nbins = mag.len();
        let ratio = self.nyq / F_LO;
        let mut col = vec![0.0f32; self.rows];
        for row in 0..self.rows {
            // Row 0 = bottom = low freq; log-spaced band [lo, hi].
            let lo = F_LO * ratio.powf(row as f32 / self.rows as f32);
            let hi = F_LO * ratio.powf((row + 1) as f32 / self.rows as f32);
            let k_lo = ((lo / self.nyq) * (nbins - 1) as f32).floor() as usize;
            let k_hi = (((hi / self.nyq) * (nbins - 1) as f32).ceil() as usize).clamp(k_lo + 1, nbins);
            let peak = mag[k_lo..k_hi].iter().cloned().fold(0.0f32, f32::max);
            let db = 20.0 * (peak + 1e-9).log10();
            col[row] = ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
        }
        self.cols.push_back(col);
        while self.cols.len() > self.cap {
            self.cols.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_columns_and_bounds_the_ring() {
        let mut s = Spectro::new(16, 4, 48_000.0);
        for _ in 0..10 {
            // a 1 kHz-ish sine hop
            let hop: Vec<f32> = (0..1920).map(|i| (i as f32 * 1000.0 * std::f32::consts::TAU / 48_000.0).sin()).collect();
            s.push_hop(&hop);
        }
        assert_eq!(s.cols().len(), 4, "ring capped at cap columns");
        assert!(s.cols().iter().all(|c| c.len() == 16), "each column has `rows` rows");
        // A tone has energy → some row lit above zero.
        assert!(s.cols().back().unwrap().iter().any(|&v| v > 0.1), "tone produced visible magnitude");
    }

    #[test]
    fn silence_is_dark() {
        let mut s = Spectro::new(8, 2, 48_000.0);
        s.push_hop(&vec![0.0f32; 1920]);
        assert!(s.cols().back().unwrap().iter().all(|&v| v <= 1e-3), "silence stays near the floor");
    }
}
