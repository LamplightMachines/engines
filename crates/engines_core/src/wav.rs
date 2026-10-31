//! WAV writers for headless render/verification.
//!
//! Not part of the I/O-free core contract — a dev convenience used by the render bins and tests.
//! The shell streams live audio via AVAudioEngine (CE-R12).
//!
//! - [`write_wav_f32`]: one-shot, whole buffer in memory, classic 44-byte RIFF header. Fine for
//!   short renders; refuses anything past the 4 GiB RIFF size limit rather than silently
//!   wrapping its `u32` sizes.
//! - [`WavWriter`]: streaming, for multi-hour songs (ADR 0029). Starts as a plain RIFF/WAVE with a
//!   28-byte `JUNK` placeholder and upgrades itself in place to **RF64** (EBU Tech 3306: the
//!   `JUNK` becomes `ds64`) when the file outgrows 4 GiB — so a 6 h f32 stereo render (~8.3 GB) is
//!   one valid file, and short renders stay ordinary WAVs every reader opens.
//! - [`WavReader`]: streams back what `WavWriter` writes (RIFF or RF64; f32/s24/s16) — the stem
//!   export (ADR 0030) reads its float temps through it.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::hash::{hash64, hash_to_unit};

/// The classic header's `(riff_size, data_size)` for `n_samples` 32-bit samples, or an error when
/// either would overflow the RIFF `u32` fields (≈ 3.1 h of 48 kHz stereo f32).
fn riff_sizes_f32(n_samples: u64) -> io::Result<(u32, u32)> {
    let data = n_samples * 4;
    let riff = 36 + data;
    if riff > u32::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{data} bytes of audio exceeds the 4 GiB RIFF limit — use wav::WavWriter (RF64) / engines-song"),
        ));
    }
    Ok((riff as u32, data as u32))
}

/// Write interleaved `samples` as a 32-bit IEEE-float WAV.
pub fn write_wav_f32(
    path: &Path,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> std::io::Result<()> {
    let bits = 32u16;
    let bytes_per_sample = (bits / 8) as u32;
    let block_align = channels as u32 * bytes_per_sample;
    let byte_rate = sample_rate * block_align;
    let (riff_bytes, data_bytes) = riff_sizes_f32(samples.len() as u64)?;

    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&riff_bytes.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    // fmt chunk
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // 3 = IEEE float
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&(block_align as u16).to_le_bytes())?;
    f.write_all(&bits.to_le_bytes())?;
    // data chunk
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for &s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

/// Output sample encoding for [`WavWriter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    /// 32-bit IEEE float, written verbatim — **no clamp** (ADR 0021: headroom is the doc's job).
    F32,
    /// 24-bit PCM: clamp to ±1, round.
    S24,
    /// 16-bit PCM: clamp to ±1, deterministic TPDF dither (±1 LSB), round.
    S16,
}

impl SampleFormat {
    /// Bytes per encoded sample (PCM container width; ×8 = bit depth).
    pub fn bytes(self) -> u16 {
        match self {
            SampleFormat::F32 => 4,
            SampleFormat::S24 => 3,
            SampleFormat::S16 => 2,
        }
    }
    fn format_tag(self) -> u16 {
        match self {
            SampleFormat::F32 => 3, // WAVE_FORMAT_IEEE_FLOAT
            _ => 1,                 // WAVE_FORMAT_PCM
        }
    }
}

impl std::str::FromStr for SampleFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "f32" => Ok(SampleFormat::F32),
            "s24" => Ok(SampleFormat::S24),
            "s16" => Ok(SampleFormat::S16),
            other => Err(format!("unknown sample format {other:?} (expected f32 | s24 | s16)")),
        }
    }
}

/// Float → integer PCM for the integer formats: clamp to ±1, round; `S16` adds the deterministic
/// TPDF dither keyed by `index` (the interleaved sample's position in the stream). Shared with
/// the FLAC writer so a `.flac` decodes to exactly the PCM the same render's `.wav` holds.
/// `F32` has no integer form — it is written verbatim, never through here.
pub(crate) fn quantize(fmt: SampleFormat, x: f32, index: u64) -> i32 {
    match fmt {
        SampleFormat::S24 => (x.clamp(-1.0, 1.0) as f64 * 8_388_607.0).round() as i32,
        SampleFormat::S16 => {
            let tpdf = hash_to_unit(hash64(index, 1, DITHER_SALT, 0)) as f64 - hash_to_unit(hash64(index, 2, DITHER_SALT, 0)) as f64;
            (x.clamp(-1.0, 1.0) as f64 * 32_767.0 + tpdf).round().clamp(-32_768.0, 32_767.0) as i32
        }
        SampleFormat::F32 => unreachable!("f32 is written verbatim, not quantized"),
    }
}

/// Peak + over-full-scale count, measured pre-quantization (both writers report it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Meter {
    pub peak: f32,
    pub clipped: u64,
}

impl Meter {
    #[inline]
    pub fn see(&mut self, x: f32) {
        let a = x.abs();
        if a > self.peak {
            self.peak = a;
        }
        if a > 1.0 {
            self.clipped += 1;
        }
    }
}

/// What [`WavWriter::finalize`] reports back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WavStats {
    pub frames: u64,
    pub data_bytes: u64,
    /// The file was upgraded to RF64 (it outgrew the RIFF 4 GiB limit).
    pub rf64: bool,
    /// Peak |sample| seen, pre-quantization (linear; may exceed 1.0 for f32).
    pub peak: f32,
    /// Samples with |s| > 1.0 (clamped for PCM formats; written as-is for f32).
    pub clipped: u64,
}

// Fixed header layout (bytes): RIFF(0) size(4) WAVE(8) | JUNK/ds64(12) size(16) body(20..48) |
// fmt (48) 16(52) body(56..72) | data(72) size(76) | samples(80…).
const DS64_AT: u64 = 12;
const DS64_BODY: u32 = 28;
const DATA_SIZE_AT: u64 = 76;
const HEADER_BYTES: u64 = 80;
/// RIFF size = file length − 8 = everything after the RIFF size field.
const RIFF_OVERHEAD: u64 = HEADER_BYTES - 8;
/// TPDF dither sub-stream salt (deterministic: same samples ⇒ same bytes).
const DITHER_SALT: u64 = 0xD17E_5EED;

/// Streaming WAV writer with automatic RF64 upgrade (see module docs).
pub struct WavWriter<W: Write + Seek> {
    w: BufWriter<W>,
    channels: u16,
    fmt: SampleFormat,
    samples: u64,
    rf64_threshold: u64,
    rf64: bool,
    meter: Meter,
    /// Stream position of this file's first sample (see [`continuing_at`](Self::continuing_at)).
    dither_base: u64,
}

impl WavWriter<File> {
    /// Create (truncate) `path` and write the provisional header.
    pub fn create(path: &Path, channels: u16, sample_rate: u32, fmt: SampleFormat) -> io::Result<Self> {
        Self::new(File::create(path)?, channels, sample_rate, fmt)
    }
}

impl<W: Write + Seek> WavWriter<W> {
    pub fn new(inner: W, channels: u16, sample_rate: u32, fmt: SampleFormat) -> io::Result<Self> {
        if channels == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "channels must be ≥ 1"));
        }
        let mut w = BufWriter::with_capacity(1 << 20, inner);
        let bps = fmt.bytes();
        let block_align = channels * bps;
        w.write_all(b"RIFF")?;
        w.write_all(&(RIFF_OVERHEAD as u32).to_le_bytes())?;
        w.write_all(b"WAVE")?;
        w.write_all(b"JUNK")?; // reserved for ds64 (EBU 3306) — readers skip unknown chunks
        w.write_all(&DS64_BODY.to_le_bytes())?;
        w.write_all(&[0u8; DS64_BODY as usize])?;
        w.write_all(b"fmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&fmt.format_tag().to_le_bytes())?;
        w.write_all(&channels.to_le_bytes())?;
        w.write_all(&sample_rate.to_le_bytes())?;
        w.write_all(&(sample_rate * block_align as u32).to_le_bytes())?;
        w.write_all(&block_align.to_le_bytes())?;
        w.write_all(&(bps * 8).to_le_bytes())?;
        w.write_all(b"data")?;
        w.write_all(&0u32.to_le_bytes())?;
        Ok(Self { w, channels, fmt, samples: 0, rf64_threshold: u32::MAX as u64, rf64: false, meter: Meter::default(), dither_base: 0 })
    }

    /// This file continues a longer stream (ADR 0043: a song written one file per block): its first
    /// sample is interleaved sample `index` of that stream. The s16 dither is keyed by stream
    /// position, so it carries on where the previous file left off — the files, laid end to end,
    /// hold exactly the PCM one file of the whole stream would. Call before writing.
    pub fn continuing_at(mut self, index: u64) -> Self {
        self.dither_base = index;
        self
    }

    /// Test hook: upgrade to RF64 once the RIFF size exceeds `bytes` (default `u32::MAX`).
    #[cfg(test)]
    fn with_rf64_threshold(mut self, bytes: u64) -> Self {
        self.rf64_threshold = bytes;
        self
    }

    #[cfg(test)]
    fn inner_ref(&self) -> &W {
        self.w.get_ref()
    }

    fn data_bytes(&self) -> u64 {
        self.samples * self.fmt.bytes() as u64
    }

    /// Append interleaved samples (any length; frames complete across calls).
    pub fn write_interleaved(&mut self, s: &[f32]) -> io::Result<()> {
        for &x in s {
            self.meter.see(x);
            match self.fmt {
                SampleFormat::F32 => self.w.write_all(&x.to_le_bytes())?,
                SampleFormat::S24 => self.w.write_all(&quantize(self.fmt, x, self.dither_base + self.samples).to_le_bytes()[..3])?,
                SampleFormat::S16 => self.w.write_all(&(quantize(self.fmt, x, self.dither_base + self.samples) as i16).to_le_bytes())?,
            }
            self.samples += 1;
        }
        Ok(())
    }

    /// Patch the header sizes for what's been written so far (RIFF or RF64), then return to the
    /// end. A process killed after a checkpoint leaves a valid, playable file.
    pub fn checkpoint(&mut self) -> io::Result<()> {
        let data = self.data_bytes();
        self.patch_header(data, RIFF_OVERHEAD + data)?;
        self.w.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn patch_header(&mut self, data: u64, riff: u64) -> io::Result<()> {
        if riff > self.rf64_threshold {
            self.rf64 = true;
        }
        let frames = self.samples / self.channels as u64;
        self.w.seek(SeekFrom::Start(0))?;
        if self.rf64 {
            self.w.write_all(b"RF64")?;
            self.w.write_all(&u32::MAX.to_le_bytes())?;
            self.w.seek(SeekFrom::Start(DS64_AT))?;
            self.w.write_all(b"ds64")?;
            self.w.write_all(&DS64_BODY.to_le_bytes())?;
            self.w.write_all(&riff.to_le_bytes())?;
            self.w.write_all(&data.to_le_bytes())?;
            self.w.write_all(&frames.to_le_bytes())?;
            self.w.write_all(&0u32.to_le_bytes())?; // table length
            self.w.seek(SeekFrom::Start(DATA_SIZE_AT))?;
            self.w.write_all(&u32::MAX.to_le_bytes())?;
        } else {
            self.w.write_all(b"RIFF")?;
            self.w.write_all(&(riff as u32).to_le_bytes())?;
            self.w.seek(SeekFrom::Start(DATA_SIZE_AT))?;
            self.w.write_all(&(data as u32).to_le_bytes())?;
        }
        Ok(())
    }

    /// Pad to an even chunk size, write the final header, flush; hand back the sink + stats.
    pub fn finalize(mut self) -> io::Result<(W, WavStats)> {
        let data = self.data_bytes();
        let pad = data % 2;
        if pad == 1 {
            self.w.write_all(&[0u8])?;
        }
        self.patch_header(data, RIFF_OVERHEAD + data + pad)?;
        self.w.flush()?;
        let stats = WavStats {
            frames: self.samples / self.channels as u64,
            data_bytes: data,
            rf64: self.rf64,
            peak: self.meter.peak,
            clipped: self.meter.clipped,
        };
        let inner = self.w.into_inner().map_err(|e| e.into_error())?;
        Ok((inner, stats))
    }
}

/// Streaming WAV reader for the files [`WavWriter`] writes: RIFF or RF64 (data size from `ds64`),
/// plain `fmt ` with IEEE float 32 or PCM 24/16. Unknown chunks (and their pad byte) are skipped.
/// PCM decodes on the writer's scale (`v / 8_388_607` · `v / 32_767`), so a round trip returns the
/// quantized value exactly.
pub struct WavReader<R: Read + Seek> {
    r: BufReader<R>,
    channels: u16,
    sample_rate: u32,
    fmt: SampleFormat,
    /// Sample-data bytes not yet read.
    remaining: u64,
    data_bytes: u64,
    buf: Vec<u8>,
}

impl WavReader<File> {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::new(File::open(path)?)
    }
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

impl<R: Read + Seek> WavReader<R> {
    pub fn new(inner: R) -> io::Result<Self> {
        let mut r = BufReader::with_capacity(1 << 20, inner);
        let mut b4 = [0u8; 4];
        let mut read_u32 = |r: &mut BufReader<R>| -> io::Result<u32> {
            r.read_exact(&mut b4)?;
            Ok(u32::from_le_bytes(b4))
        };
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        let rf64 = match &magic {
            b"RIFF" => false,
            b"RF64" => true,
            _ => return Err(bad("not a RIFF/RF64 file")),
        };
        read_u32(&mut r)?; // RIFF size (RF64: 0xFFFFFFFF — the real one is in ds64)
        r.read_exact(&mut magic)?;
        if &magic != b"WAVE" {
            return Err(bad("not a WAVE file"));
        }
        let (mut ds64_data, mut fmt) = (None, None);
        loop {
            let mut id = [0u8; 4];
            r.read_exact(&mut id)?;
            let size = read_u32(&mut r)?;
            match &id {
                b"ds64" => {
                    let mut body = vec![0u8; size as usize];
                    r.read_exact(&mut body)?;
                    if body.len() < 16 {
                        return Err(bad("short ds64 chunk"));
                    }
                    ds64_data = Some(u64::from_le_bytes(body[8..16].try_into().unwrap()));
                }
                b"fmt " => {
                    let mut body = vec![0u8; size as usize];
                    r.read_exact(&mut body)?;
                    if body.len() < 16 {
                        return Err(bad("short fmt chunk"));
                    }
                    let u16_at = |at: usize| u16::from_le_bytes([body[at], body[at + 1]]);
                    let (tag, ch, bits) = (u16_at(0), u16_at(2), u16_at(14));
                    let sr = u32::from_le_bytes(body[4..8].try_into().unwrap());
                    let f = match (tag, bits) {
                        (3, 32) => SampleFormat::F32,
                        (1, 24) => SampleFormat::S24,
                        (1, 16) => SampleFormat::S16,
                        _ => return Err(bad(format!("unsupported WAV encoding (format tag {tag}, {bits}-bit)"))),
                    };
                    if ch == 0 {
                        return Err(bad("0 channels"));
                    }
                    fmt = Some((f, ch, sr));
                }
                b"data" => {
                    let (fmt, channels, sample_rate) = fmt.ok_or_else(|| bad("data chunk before fmt"))?;
                    let data_bytes = match (rf64, size) {
                        (true, u32::MAX) => ds64_data.ok_or_else(|| bad("RF64 without ds64"))?,
                        _ => size as u64,
                    };
                    return Ok(Self { r, channels, sample_rate, fmt, remaining: data_bytes, data_bytes, buf: Vec::new() });
                }
                _ => {
                    r.seek_relative(size as i64 + (size % 2) as i64)?;
                }
            }
            if size % 2 == 1 && matches!(&id, b"ds64" | b"fmt ") {
                r.seek_relative(1)?;
            }
        }
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn format(&self) -> SampleFormat {
        self.fmt
    }
    /// Total frames in the data chunk.
    pub fn frames(&self) -> u64 {
        self.data_bytes / (self.channels as u64 * self.fmt.bytes() as u64)
    }

    /// Read up to `out.len()` interleaved samples; `Ok(0)` at the end of the data. A file cut short
    /// of its declared data size is an `UnexpectedEof` error, not a silent short read.
    pub fn read_interleaved(&mut self, out: &mut [f32]) -> io::Result<usize> {
        let bps = self.fmt.bytes() as usize;
        let n = out.len().min((self.remaining / bps as u64) as usize);
        if n == 0 {
            return Ok(0);
        }
        self.buf.resize(n * bps, 0);
        self.r.read_exact(&mut self.buf)?;
        self.remaining -= (n * bps) as u64;
        for (o, c) in out.iter_mut().zip(self.buf.chunks_exact(bps)) {
            *o = match self.fmt {
                SampleFormat::F32 => f32::from_le_bytes(c.try_into().unwrap()),
                SampleFormat::S24 => (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_607.0,
                SampleFormat::S16 => i16::from_le_bytes([c[0], c[1]]) as f32 / 32_767.0,
            };
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn writes_readable_riff_header() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("engines_wav_test_{}.wav", std::process::id()));
        let _ = std::fs::remove_file(&path);
        write_wav_f32(&path, &[0.0, 0.1, -0.1, 0.2], 2, 48_000).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        // 44-byte header + 4 samples * 4 bytes
        assert_eq!(bytes.len(), 44 + 16);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn one_shot_writer_refuses_past_the_riff_limit() {
        // ≈ 3.1 h of 48 kHz stereo f32 is the most a classic header can describe.
        assert!(riff_sizes_f32(1_000).is_ok());
        let max_ok = (u32::MAX as u64 - 36) / 4;
        assert!(riff_sizes_f32(max_ok).is_ok());
        let err = riff_sizes_f32(max_ok + 1).unwrap_err();
        assert!(err.to_string().contains("RF64"), "{err}");
        // 6 h of stereo 48 kHz is far past it.
        assert!(riff_sizes_f32(6 * 3600 * 48_000 * 2).is_err());
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }
    fn u64_at(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    /// Minimal chunk walker: (is_rf64, fmt tag, channels, bits, data bytes slice).
    fn parse(b: &[u8]) -> (bool, u16, u16, u16, Vec<u8>) {
        let rf64 = match &b[0..4] {
            b"RIFF" => false,
            b"RF64" => true,
            other => panic!("bad magic {other:?}"),
        };
        assert_eq!(&b[8..12], b"WAVE");
        if !rf64 {
            assert_eq!(u32_at(b, 4) as usize, b.len() - 8, "RIFF size = file len − 8");
        }
        let mut at = 12;
        let (mut tag, mut ch, mut bits, mut ds64_data) = (0, 0, 0, None);
        loop {
            let id = &b[at..at + 4];
            let size = u32_at(b, at + 4) as usize;
            let body = at + 8;
            match id {
                b"ds64" => {
                    assert!(rf64, "ds64 only in RF64");
                    assert_eq!(u64_at(b, body) as usize, b.len() - 8, "ds64 riffSize");
                    ds64_data = Some(u64_at(b, body + 8) as usize);
                }
                b"fmt " => {
                    tag = u16::from_le_bytes([b[body], b[body + 1]]);
                    ch = u16::from_le_bytes([b[body + 2], b[body + 3]]);
                    bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
                }
                b"data" => {
                    let n = if rf64 {
                        assert_eq!(size as u32, u32::MAX);
                        ds64_data.expect("ds64 before data")
                    } else {
                        size
                    };
                    return (rf64, tag, ch, bits, b[body..body + n].to_vec());
                }
                _ => {}
            }
            at = body + size + (size % 2);
        }
    }

    fn render(fmt: SampleFormat, ch: u16, s: &[f32]) -> (Vec<u8>, WavStats) {
        let mut w = WavWriter::new(Cursor::new(Vec::new()), ch, 48_000, fmt).unwrap();
        // two calls: frames must stitch across write boundaries
        let mid = s.len() / 2;
        w.write_interleaved(&s[..mid]).unwrap();
        w.write_interleaved(&s[mid..]).unwrap();
        let (c, st) = w.finalize().unwrap();
        (c.into_inner(), st)
    }

    #[test]
    fn streaming_f32_round_trips_and_keeps_overs() {
        let s = [0.0f32, 0.25, -0.5, 1.5, -0.75, 0.125];
        let (b, st) = render(SampleFormat::F32, 2, &s);
        let (rf64, tag, ch, bits, data) = parse(&b);
        assert_eq!((rf64, tag, ch, bits), (false, 3, 2, 32));
        assert_eq!(&b[12..16], b"JUNK");
        let back: Vec<f32> = data.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(back, s, "f32 is verbatim — no clamp (ADR 0021)");
        assert_eq!((st.frames, st.data_bytes, st.rf64, st.clipped), (3, 24, false, 1));
        assert_eq!(st.peak, 1.5);
    }

    #[test]
    fn streaming_s24_quantizes_and_clamps() {
        let s = [0.5f32, -1.0, 2.0, 0.0];
        let (b, st) = render(SampleFormat::S24, 2, &s);
        let (_, tag, _, bits, data) = parse(&b);
        assert_eq!((tag, bits), (1, 24));
        let back: Vec<i32> = data.chunks(3).map(|c| i32::from_le_bytes([c[0], c[1], c[2], if c[2] & 0x80 != 0 { 0xFF } else { 0 }])).collect();
        assert_eq!(back, vec![4_194_304, -8_388_607, 8_388_607, 0]);
        assert_eq!(st.clipped, 1);
    }

    #[test]
    fn s24_mono_odd_data_is_padded() {
        let (b, st) = render(SampleFormat::S24, 1, &[0.1]);
        assert_eq!(st.data_bytes, 3);
        assert_eq!(b.len() as u64, HEADER_BYTES + 4, "one pad byte after odd data");
        let (_, _, ch, _, data) = parse(&b);
        assert_eq!((ch, data.len()), (1, 3));
    }

    #[test]
    fn s16_dither_is_deterministic_and_within_one_lsb() {
        let s: Vec<f32> = (0..512).map(|i| ((i as f32) * 0.037).sin() * 0.6).collect();
        let (a, _) = render(SampleFormat::S16, 2, &s);
        let (b, _) = render(SampleFormat::S16, 2, &s);
        assert_eq!(a, b, "TPDF dither is hashed, not random");
        let (_, tag, _, bits, data) = parse(&a);
        assert_eq!((tag, bits), (1, 16));
        let mut dithered = 0;
        for (i, c) in data.chunks(2).enumerate() {
            let v = i16::from_le_bytes([c[0], c[1]]) as f64;
            let ideal = s[i] as f64 * 32_767.0;
            assert!((v - ideal).abs() <= 1.5, "sample {i}: {v} vs {ideal}");
            if (v - ideal.round()).abs() > 0.0 {
                dithered += 1;
            }
        }
        assert!(dithered > 0, "dither must actually move some samples");
    }

    #[test]
    fn a_continuing_file_carries_the_dither_on() {
        // A stream split in two files (ADR 0043) holds exactly the PCM of one file of it.
        let s: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.021).sin() * 0.6).collect();
        let (whole, _) = render(SampleFormat::S16, 2, &s);
        let cut = 602; // whole frames: 301 of them
        let part = |s: &[f32], at: u64| {
            let mut w = WavWriter::new(Cursor::new(Vec::new()), 2, 48_000, SampleFormat::S16).unwrap().continuing_at(at);
            w.write_interleaved(s).unwrap();
            parse(&w.finalize().unwrap().0.into_inner()).4
        };
        let joined = [part(&s[..cut], 0), part(&s[cut..], cut as u64)].concat();
        assert_eq!(joined, parse(&whole).4);
        assert_ne!(part(&s[cut..], 0), parse(&whole).4[cut * 2..], "a fresh file would restart the dither");
    }

    #[test]
    fn upgrades_to_rf64_past_the_threshold() {
        let s: Vec<f32> = (0..64).map(|i| i as f32 / 64.0).collect();
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 2, 48_000, SampleFormat::F32).unwrap().with_rf64_threshold(100);
        w.write_interleaved(&s).unwrap();
        let (c, st) = w.finalize().unwrap();
        let b = c.into_inner();
        assert!(st.rf64);
        assert_eq!(&b[0..4], b"RF64");
        assert_eq!(u32_at(&b, 4), u32::MAX);
        assert_eq!(&b[12..16], b"ds64");
        assert_eq!(u64_at(&b, 20), 72 + 256, "ds64 riffSize");
        assert_eq!(u64_at(&b, 28), 256, "ds64 dataSize");
        assert_eq!(u64_at(&b, 36), 32, "ds64 sampleCount = frames");
        assert_eq!(u32_at(&b, DATA_SIZE_AT as usize), u32::MAX);
        let (rf64, _, _, _, data) = parse(&b);
        assert!(rf64);
        assert_eq!(data.len(), 256);
    }

    #[test]
    fn checkpoint_leaves_a_parseable_file_and_writing_continues() {
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 2, 48_000, SampleFormat::F32).unwrap();
        w.write_interleaved(&[0.1, 0.2, 0.3, 0.4]).unwrap();
        w.checkpoint().unwrap();
        let mid = w.inner_ref().get_ref().clone();
        let (_, _, _, _, data) = parse(&mid);
        assert_eq!(data.len(), 16, "checkpointed header describes what's been written");
        w.write_interleaved(&[0.5, 0.6]).unwrap();
        let (c, st) = w.finalize().unwrap();
        let (_, _, _, _, data) = parse(c.get_ref());
        assert_eq!((data.len(), st.frames), (24, 3));
    }

    fn read_all<R: Read + Seek>(mut rd: WavReader<R>, chunk: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = vec![0.0f32; chunk];
        loop {
            let n = rd.read_interleaved(&mut buf).unwrap();
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    #[test]
    fn reader_round_trips_riff_and_rf64() {
        let s: Vec<f32> = (0..400).map(|i| ((i as f32) * 0.05).sin() * 1.3).collect(); // incl. overs
        let (b, _) = render(SampleFormat::F32, 2, &s);
        let rd = WavReader::new(Cursor::new(b)).unwrap();
        assert_eq!((rd.channels(), rd.sample_rate(), rd.format(), rd.frames()), (2, 48_000, SampleFormat::F32, 200));
        assert_eq!(read_all(rd, 7), s, "odd read sizes, f32 verbatim");

        let mut w = WavWriter::new(Cursor::new(Vec::new()), 2, 48_000, SampleFormat::F32).unwrap().with_rf64_threshold(100);
        w.write_interleaved(&s).unwrap();
        let (c, st) = w.finalize().unwrap();
        assert!(st.rf64);
        let rd = WavReader::new(Cursor::new(c.into_inner())).unwrap();
        assert_eq!(rd.frames(), 200);
        assert_eq!(read_all(rd, 64), s, "RF64: data size from ds64");
    }

    #[test]
    fn reader_decodes_pcm_on_the_writer_scale() {
        let s: Vec<f32> = (0..300).map(|i| ((i as f32) * 0.07).sin() * 0.8).collect();
        for (fmt, scale) in [(SampleFormat::S24, 8_388_607.0f32), (SampleFormat::S16, 32_767.0)] {
            let (b, _) = render(fmt, 2, &s);
            let back = read_all(WavReader::new(Cursor::new(b)).unwrap(), 33);
            let want: Vec<f32> = s.iter().enumerate().map(|(i, &x)| quantize(fmt, x, i as u64) as f32 / scale).collect();
            assert_eq!(back, want, "{fmt:?}");
        }
    }

    #[test]
    fn reader_skips_unknown_and_padded_chunks() {
        let mut b: Vec<u8> = Vec::new();
        b.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        b.extend_from_slice(b"LIST");
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&[1, 2, 3, 0]); // odd body + pad byte
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        for v in [3u16, 1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&44_100u32.to_le_bytes());
        b.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        for v in [4u16, 32] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(b"data");
        b.extend_from_slice(&8u32.to_le_bytes());
        for x in [0.5f32, -0.25] {
            b.extend_from_slice(&x.to_le_bytes());
        }
        let rd = WavReader::new(Cursor::new(b)).unwrap();
        assert_eq!((rd.channels(), rd.sample_rate()), (1, 44_100));
        assert_eq!(read_all(rd, 16), vec![0.5, -0.25]);
    }

    #[test]
    fn reader_rejects_bad_and_truncated_files() {
        assert!(WavReader::new(Cursor::new(b"RIFX\0\0\0\0WAVE".to_vec())).is_err());
        let (mut b, _) = render(SampleFormat::F32, 2, &[0.1; 64]);
        b.truncate(b.len() - 10);
        let mut rd = WavReader::new(Cursor::new(b)).unwrap();
        let mut buf = vec![0.0f32; 64];
        assert_eq!(rd.read_interleaved(&mut buf).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn sample_format_parses() {
        assert_eq!("s24".parse::<SampleFormat>(), Ok(SampleFormat::S24));
        assert!("wav".parse::<SampleFormat>().is_err());
    }
}
