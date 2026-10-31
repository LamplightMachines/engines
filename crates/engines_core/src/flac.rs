//! Streaming FLAC writer for headless song renders (`engines-song --out night.flac`; feature `flac`).
//!
//! Not part of the I/O-free core contract — a dev convenience like [`crate::wav`]. FLAC is
//! lossless **integer** PCM: [`FlacWriter`] quantizes through the same `wav::quantize` (incl. the
//! hashed s16 TPDF dither) as [`WavWriter`](crate::wav::WavWriter), so a `.flac` decodes
//! bit-for-bit to the PCM of the matching `--format s24|s16` `.wav`. There is no float FLAC, so
//! [`SampleFormat::F32`] is refused; overs clamp (and are counted) exactly as for an s24 WAV.
//!
//! Encoding is the pure-Rust `flacenc` crate (default settings ≈ `flac -5`), one fixed
//! 4096-frame block at a time, so memory stays flat over a 6 h render and there is no size limit
//! to outgrow (STREAMINFO's 36-bit sample count covers ~400 h @ 48 kHz — no RF64 analogue
//! needed). The 42-byte `fLaC` + STREAMINFO header is rewritten in place at each
//! [`checkpoint`](FlacWriter::checkpoint) (sample count + frame sizes, MD5 left unset because
//! more frames follow — a killed render is still a playable file) and at
//! [`finalize`](FlacWriter::finalize), which adds the MD5 of the whole stream.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::config;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context, Fill, FrameBuf};

use crate::wav::{quantize, Meter, SampleFormat};

/// Inter-channel samples per FLAC frame (the reference encoder's default).
const BLOCK: usize = 4096;
/// `fLaC` + the STREAMINFO metadata block header + its 34-byte body.
const HEADER_BYTES: u64 = 42;

/// What [`FlacWriter::finalize`] reports back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlacStats {
    pub frames: u64,
    /// Encoded file size in bytes (header + frames).
    pub bytes: u64,
    /// Peak |sample| seen, pre-quantization (linear; may exceed 1.0).
    pub peak: f32,
    /// Samples with |s| > 1.0 (clamped to full scale).
    pub clipped: u64,
}

/// Streaming FLAC writer (see module docs).
pub struct FlacWriter<W: Write + Seek> {
    w: BufWriter<W>,
    config: Verified<config::Encoder>,
    info: StreamInfo,
    fmt: SampleFormat,
    channels: usize,
    fb: FrameBuf,
    /// MD5 + sample count over exactly the samples that went into frames.
    ctx: Context,
    /// Quantized interleaved PCM not yet encoded (under one block, or a partial frame).
    pending: Vec<i32>,
    sink: ByteSink,
    /// Interleaved samples seen — the s16 dither index, same as `WavWriter`'s.
    samples: u64,
    frame_no: usize,
    bytes: u64,
    meter: Meter,
    /// Stream position of this file's first sample (see [`continuing_at`](Self::continuing_at)).
    dither_base: u64,
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl FlacWriter<File> {
    /// Create (truncate) `path` and write the provisional header.
    pub fn create(path: &Path, channels: u16, sample_rate: u32, fmt: SampleFormat) -> io::Result<Self> {
        Self::new(File::create(path)?, channels, sample_rate, fmt)
    }
}

impl<W: Write + Seek> FlacWriter<W> {
    pub fn new(inner: W, channels: u16, sample_rate: u32, fmt: SampleFormat) -> io::Result<Self> {
        if fmt == SampleFormat::F32 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "FLAC is integer PCM — use s24 or s16, not f32"));
        }
        let ch = channels as usize;
        let bits = fmt.bytes() as usize * 8;
        // Verifies channels (1..=8) and the sample rate (FLAC's 20-bit field).
        let info = StreamInfo::new(sample_rate as usize, ch, bits).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let mut me = Self {
            w: BufWriter::with_capacity(1 << 20, inner),
            config: config::Encoder::default().into_verified().map_err(|(_, e)| other(e))?,
            info,
            fmt,
            channels: ch,
            fb: FrameBuf::with_size(ch, BLOCK).map_err(other)?,
            ctx: Context::new(bits, ch),
            pending: Vec::with_capacity(2 * BLOCK * ch),
            sink: ByteSink::new(),
            samples: 0,
            frame_no: 0,
            bytes: HEADER_BYTES,
            meter: Meter::default(),
            dither_base: 0,
        };
        me.write_header(false)?;
        Ok(me)
    }

    /// This file continues a longer stream, starting at its interleaved sample `index` — the s16
    /// dither carries on, as [`WavWriter::continuing_at`](crate::wav::WavWriter::continuing_at).
    pub fn continuing_at(mut self, index: u64) -> Self {
        self.dither_base = index;
        self
    }

    #[cfg(test)]
    fn inner_ref(&self) -> &W {
        self.w.get_ref()
    }

    /// `fLaC` + STREAMINFO describing the frames encoded so far (always [`HEADER_BYTES`] long).
    fn header(&self, with_md5: bool) -> io::Result<Vec<u8>> {
        let mut info = self.info.clone();
        // Fixed-blocksize stream: min/max block size exclude the short last block (FLAC spec).
        info.set_block_sizes(BLOCK, BLOCK).map_err(other)?;
        if self.frame_no == 0 {
            info.set_frame_sizes(0, 0).map_err(other)?; // 0 = unknown (flacenc starts at MAX..0)
        }
        if with_md5 {
            info.set_md5_digest(&self.ctx.md5_digest());
        }
        let mut sink = ByteSink::new();
        Stream::with_stream_info(info).write(&mut sink).map_err(other)?;
        let bytes = sink.into_inner();
        debug_assert_eq!(bytes.len() as u64, HEADER_BYTES);
        Ok(bytes)
    }

    fn write_header(&mut self, with_md5: bool) -> io::Result<()> {
        let h = self.header(with_md5)?;
        self.w.seek(SeekFrom::Start(0))?;
        self.w.write_all(&h)?;
        self.w.seek(SeekFrom::End(0))?;
        Ok(())
    }

    /// Encode `pending[at..at + len]` (whole inter-channel frames, ≤ one block) as one FLAC frame.
    fn encode(&mut self, at: usize, len: usize) -> io::Result<()> {
        let block = &self.pending[at..at + len];
        self.fb.fill_interleaved(block).map_err(other)?;
        self.ctx.fill_interleaved(block).map_err(other)?;
        let frame = flacenc::encode_fixed_size_frame(&self.config, &self.fb, self.frame_no, &self.info).map_err(other)?;
        self.info.update_frame_info(&frame);
        self.sink.clear();
        frame.write(&mut self.sink).map_err(other)?;
        self.w.write_all(self.sink.as_slice())?;
        self.bytes += self.sink.as_slice().len() as u64;
        self.frame_no += 1;
        Ok(())
    }

    /// Append interleaved samples (any length; frames complete across calls).
    pub fn write_interleaved(&mut self, s: &[f32]) -> io::Result<()> {
        for &x in s {
            self.meter.see(x);
            self.pending.push(quantize(self.fmt, x, self.dither_base + self.samples));
            self.samples += 1;
        }
        let per_block = BLOCK * self.channels;
        let mut at = 0;
        while self.pending.len() - at >= per_block {
            self.encode(at, per_block)?;
            at += per_block;
        }
        self.pending.drain(..at);
        Ok(())
    }

    /// Rewrite STREAMINFO for the frames encoded so far (MD5 unset — more frames follow), then
    /// return to the end. A process killed after a checkpoint leaves a valid, playable file.
    pub fn checkpoint(&mut self) -> io::Result<()> {
        self.write_header(false)
    }

    /// Encode the short last block, write the final STREAMINFO (with MD5), flush; hand back the
    /// sink + stats. A trailing partial inter-channel frame can't be encoded and is dropped (the
    /// session only ever stops on whole frames).
    pub fn finalize(mut self) -> io::Result<(W, FlacStats)> {
        let whole = self.pending.len() / self.channels * self.channels;
        if whole > 0 {
            self.encode(0, whole)?;
        }
        self.write_header(true)?;
        self.w.flush()?;
        let stats = FlacStats { frames: self.info.total_samples() as u64, bytes: self.bytes, peak: self.meter.peak, clipped: self.meter.clipped };
        let inner = self.w.into_inner().map_err(|e| e.into_error())?;
        Ok((inner, stats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::WavWriter;
    use md5::{Digest, Md5};
    use std::io::Cursor;

    /// `frames` of a stereo signal under full scale (peak < 0.95).
    fn signal(frames: usize) -> Vec<f32> {
        (0..frames * 2).map(|i| ((i as f32) * 0.0123).sin() * 0.7 + ((i as f32) * 0.37).sin() * 0.25).collect()
    }

    fn encode(fmt: SampleFormat, ch: u16, s: &[f32]) -> (Vec<u8>, FlacStats) {
        let mut w = FlacWriter::new(Cursor::new(Vec::new()), ch, 48_000, fmt).unwrap();
        // Odd-sized writes: frames and blocks must stitch across calls.
        for chunk in s.chunks(3001) {
            w.write_interleaved(chunk).unwrap();
        }
        let (c, st) = w.finalize().unwrap();
        (c.into_inner(), st)
    }

    /// Decode with an independent decoder (claxon): (streaminfo, interleaved PCM).
    fn decode(b: &[u8]) -> (claxon::metadata::StreamInfo, Vec<i32>) {
        let mut r = claxon::FlacReader::new(Cursor::new(b)).expect("valid FLAC");
        let info = r.streaminfo();
        let pcm = r.samples().collect::<Result<Vec<i32>, _>>().expect("decodes");
        (info, pcm)
    }

    /// The integer PCM a `WavWriter` of the same format writes for `s`.
    fn wav_pcm(fmt: SampleFormat, s: &[f32]) -> Vec<i32> {
        let mut w = WavWriter::new(Cursor::new(Vec::new()), 2, 48_000, fmt).unwrap();
        w.write_interleaved(s).unwrap();
        let (c, _) = w.finalize().unwrap();
        let b = c.into_inner();
        let data = &b[80..80 + s.len() * fmt.bytes() as usize];
        match fmt {
            SampleFormat::S24 => data.chunks(3).map(|c| i32::from_le_bytes([c[0], c[1], c[2], if c[2] & 0x80 != 0 { 0xFF } else { 0 }])).collect(),
            SampleFormat::S16 => data.chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as i32).collect(),
            SampleFormat::F32 => unreachable!(),
        }
    }

    fn md5_of(pcm: &[i32], bytes_per_sample: usize) -> [u8; 16] {
        let mut h = Md5::new();
        for v in pcm {
            h.update(&v.to_le_bytes()[..bytes_per_sample]);
        }
        h.finalize().into()
    }

    #[test]
    fn s24_is_lossless_and_matches_the_wav_pcm() {
        let mut s = signal(3 * BLOCK + 777);
        s[100] = 1.5; // an over: clamped + counted, like s24 WAV
        let (b, st) = encode(SampleFormat::S24, 2, &s);
        assert_eq!(&b[..4], b"fLaC");
        let (info, pcm) = decode(&b);
        let want = wav_pcm(SampleFormat::S24, &s);
        assert_eq!(pcm, want, "decoded FLAC == the s24 WAV's PCM, bit for bit");
        assert_eq!((info.sample_rate, info.channels, info.bits_per_sample), (48_000, 2, 24));
        assert_eq!(info.samples, Some((3 * BLOCK + 777) as u64));
        assert_eq!((info.min_block_size, info.max_block_size), (BLOCK as u16, BLOCK as u16));
        assert!(info.min_frame_size.is_some_and(|m| m > 0) && info.max_frame_size.is_some());
        assert_eq!(info.md5sum, md5_of(&want, 3), "STREAMINFO MD5 = MD5 of the PCM (what `flac -t` checks)");
        assert_eq!((st.frames, st.bytes, st.clipped, st.peak), ((3 * BLOCK + 777) as u64, b.len() as u64, 1, 1.5));
        assert!(st.bytes < (s.len() * 3) as u64, "it actually compresses");
    }

    #[test]
    fn s16_carries_the_same_dither_as_the_wav() {
        let s = signal(BLOCK + 10);
        let (b, _) = encode(SampleFormat::S16, 2, &s);
        let (info, pcm) = decode(&b);
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(pcm, wav_pcm(SampleFormat::S16, &s), "same hashed TPDF dither ⇒ same PCM");
        assert_eq!(encode(SampleFormat::S16, 2, &s).0, b, "deterministic bytes");
    }

    #[test]
    fn a_continuing_file_carries_the_dither_on() {
        let s = signal(BLOCK + 10);
        let cut = 2 * (BLOCK / 2 + 3);
        let part = |s: &[f32], at: u64| {
            let mut w = FlacWriter::new(Cursor::new(Vec::new()), 2, 48_000, SampleFormat::S16).unwrap().continuing_at(at);
            w.write_interleaved(s).unwrap();
            decode(&w.finalize().unwrap().0.into_inner()).1
        };
        assert_eq!([part(&s[..cut], 0), part(&s[cut..], cut as u64)].concat(), wav_pcm(SampleFormat::S16, &s), "the halves hold the whole stream's PCM");
    }

    #[test]
    fn mono_and_sub_block_streams_decode() {
        let s: Vec<f32> = (0..100).map(|i| i as f32 / 200.0).collect();
        let (b, st) = encode(SampleFormat::S24, 1, &s);
        let (info, pcm) = decode(&b);
        assert_eq!((info.channels, info.samples, pcm.len(), st.frames), (1, Some(100), 100, 100));
    }

    #[test]
    fn empty_stream_is_a_valid_file() {
        let (b, st) = encode(SampleFormat::S24, 2, &[]);
        assert_eq!(b.len() as u64, HEADER_BYTES);
        let (info, pcm) = decode(&b);
        assert_eq!((info.samples, pcm.len(), st.frames), (None, 0, 0), "0 samples reads as 'unknown' — still valid");
    }

    #[test]
    fn checkpoint_leaves_a_decodable_file_and_writing_continues() {
        let s = signal(2 * BLOCK + 50);
        let mut w = FlacWriter::new(Cursor::new(Vec::new()), 2, 48_000, SampleFormat::S24).unwrap();
        w.write_interleaved(&s[..(BLOCK + 50) * 2]).unwrap();
        w.checkpoint().unwrap();
        let mid = w.inner_ref().get_ref().clone();
        let (info, pcm) = decode(&mid);
        assert_eq!(info.samples, Some(BLOCK as u64), "describes the frames encoded so far (the 50 pending aren't yet)");
        assert_eq!(info.md5sum, [0; 16], "MD5 unset until finalize");
        assert_eq!(pcm.len(), BLOCK * 2);
        w.write_interleaved(&s[(BLOCK + 50) * 2..]).unwrap();
        let (c, st) = w.finalize().unwrap();
        let (info, pcm) = decode(c.get_ref());
        assert_eq!((info.samples, st.frames), (Some((2 * BLOCK + 50) as u64), (2 * BLOCK + 50) as u64));
        assert_eq!(pcm, wav_pcm(SampleFormat::S24, &s));
    }

    #[test]
    fn refuses_float() {
        let err = FlacWriter::new(Cursor::new(Vec::new()), 2, 48_000, SampleFormat::F32).err().expect("f32 refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(FlacWriter::new(Cursor::new(Vec::new()), 9, 48_000, SampleFormat::S24).is_err(), "FLAC caps at 8 channels");
    }
}
