//! `system-audio-recorder <out.wav>` — record whatever macOS is playing to a 32-bit float WAV, showing
//! the elapsed time, until Enter (or Ctrl-C). Run it through `utils/record-system-audio`.
//!
//! How: a Core Audio **process tap** (macOS 14.2+) — a private, global stereo mixdown of every
//! process's output — inside a private aggregate device whose IO block hands each buffer to the main
//! thread, which writes the WAV. No loopback device or routing is needed. macOS asks once for
//! "System Audio Recording" permission for the app that launched it (the terminal); without it the
//! tap delivers silence, so the meter shows the level to make that obvious.

use std::ffi::{c_void, CStr};
use std::io::{BufRead, Write};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat, AudioDeviceCreateIOProcIDWithBlock,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    CATapDescription,
};
use objc2_core_audio_types::{kAudioFormatFlagIsFloat, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};

/// Seconds of all-zero input after which we suspect the permission is missing.
const SILENCE_HINT_S: f64 = 3.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = match args.as_slice() {
        [p] if p != "-h" && p != "--help" => p.clone(),
        _ => {
            eprintln!("usage: system-audio-recorder <out.wav>\n\nRecords everything macOS is playing until you press Enter (or Ctrl-C).");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(&out) {
        eprintln!("\nerror: {e}");
        std::process::exit(1);
    }
}

/// The tap, the aggregate device around it and the IO proc — torn down in reverse on drop.
struct Capture {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    running: bool,
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            if self.running {
                AudioDeviceStop(self.aggregate, self.io_proc);
            }
            if self.io_proc.is_some() {
                AudioDeviceDestroyIOProcID(self.aggregate, self.io_proc);
            }
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }
}

fn run(out: &str) -> Result<(), String> {
    let mut cap = Capture { tap: 0, aggregate: 0, io_proc: None, running: false };

    // 1. A private global stereo tap: every process's output, none excluded, left audible.
    let tap_uuid = NSUUID::UUID();
    unsafe {
        let desc = CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &NSArray::new());
        desc.setUUID(&tap_uuid);
        desc.setName(&NSString::from_str("record-system-audio"));
        desc.setPrivate(true);
        check(AudioHardwareCreateProcessTap(Some(&desc), &mut cap.tap), "create the system-audio tap")?;
    }
    let fmt: AudioStreamBasicDescription = unsafe { get_property(cap.tap, kAudioTapPropertyFormat) }
        .map_err(|s| format!("read the tap's format ({})", status(s)))?;
    let is_f32 = fmt.mFormatFlags & kAudioFormatFlagIsFloat != 0 && fmt.mBitsPerChannel == 32;
    if !is_f32 {
        return Err(format!("unexpected tap format: flags {:#x}, {} bits", fmt.mFormatFlags, fmt.mBitsPerChannel));
    }
    let channels = fmt.mChannelsPerFrame.max(1) as u16;
    let rate = fmt.mSampleRate;

    // 2. A private aggregate device whose only input is the tap (it starts the tap itself).
    let yes = NSNumber::numberWithBool(true);
    let no = NSNumber::numberWithBool(false);
    let tap_uid = tap_uuid.UUIDString();
    let tap_entry: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(
        &[&*key(kAudioSubTapUIDKey), &*key(kAudioSubTapDriftCompensationKey)],
        &[any(&*tap_uid), any(&*yes)],
    );
    let taps = NSArray::from_retained_slice(&[tap_entry]);
    let no_subdevices: Retained<NSArray<AnyObject>> = NSArray::new();
    let name = NSString::from_str("record-system-audio");
    let agg_uid = NSUUID::UUID().UUIDString();
    let agg: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(
        &[
            &*key(kAudioAggregateDeviceNameKey),
            &*key(kAudioAggregateDeviceUIDKey),
            &*key(kAudioAggregateDeviceIsPrivateKey),
            &*key(kAudioAggregateDeviceIsStackedKey),
            &*key(kAudioAggregateDeviceTapAutoStartKey),
            &*key(kAudioAggregateDeviceSubDeviceListKey),
            &*key(kAudioAggregateDeviceTapListKey),
        ],
        &[any(&*name), any(&*agg_uid), any(&*yes), any(&*no), any(&*yes), any(&*no_subdevices), any(&*taps)],
    );
    // NSDictionary is toll-free bridged to CFDictionary.
    let agg_cf: &CFDictionary = unsafe { &*(Retained::as_ptr(&agg) as *const CFDictionary) };
    check(
        unsafe { AudioHardwareCreateAggregateDevice(agg_cf, NonNull::from(&mut cap.aggregate)) },
        "create the capture device",
    )?;

    // 3. The IO block: copy each input buffer out (interleaved f32) and hand it to the main thread.
    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(512);
    let dropped = Arc::new(AtomicUsize::new(0));
    let dropped_io = dropped.clone();
    let block = RcBlock::new(
        move |_now: NonNull<AudioTimeStamp>,
              input: NonNull<AudioBufferList>,
              _in_time: NonNull<AudioTimeStamp>,
              _output: NonNull<AudioBufferList>,
              _out_time: NonNull<AudioTimeStamp>| {
            let list = unsafe { input.as_ref() };
            let n = list.mNumberBuffers as usize;
            if n == 0 {
                return;
            }
            let bufs = unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), n) };
            let chunk = if n == 1 {
                let b = &bufs[0];
                if b.mData.is_null() {
                    return;
                }
                unsafe { std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4) }.to_vec()
            } else {
                // Non-interleaved: one mono buffer per channel.
                let frames = bufs[0].mDataByteSize as usize / 4;
                let mut v = Vec::with_capacity(frames * n);
                for f in 0..frames {
                    for b in bufs {
                        v.push(if b.mData.is_null() { 0.0 } else { unsafe { *(b.mData as *const f32).add(f) } });
                    }
                }
                v
            };
            if tx.try_send(chunk).is_err() {
                dropped_io.fetch_add(1, Ordering::Relaxed);
            }
        },
    );
    check(
        unsafe {
            AudioDeviceCreateIOProcIDWithBlock(
                NonNull::from(&mut cap.io_proc),
                cap.aggregate,
                None,
                RcBlock::as_ptr(&block),
            )
        },
        "attach to the capture device",
    )?;

    // 4. Write the WAV while we wait for Enter / Ctrl-C.
    let spec = hound::WavSpec {
        // A non-interleaved tap is re-interleaved in the IO block, one buffer per channel.
        channels,
        sample_rate: rate.round() as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut wav = hound::WavWriter::create(out, spec).map_err(|e| format!("create {out}: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let on_enter = stop.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line); // Enter (or end of input)
        on_enter.store(true, Ordering::SeqCst);
    });
    let on_ctrlc = stop.clone();
    ctrlc::set_handler(move || on_ctrlc.store(true, Ordering::SeqCst)).map_err(|e| format!("Ctrl-C handler: {e}"))?;

    check(unsafe { AudioDeviceStart(cap.aggregate, cap.io_proc) }, "start recording")?;
    cap.running = true;
    eprintln!("Recording system audio → {out}  ({} ch, {} Hz, 32-bit float). Press Enter to stop.", channels, spec.sample_rate);

    let started = Instant::now();
    let mut frames: u64 = 0;
    let mut window_peak = 0f32;
    let mut heard = false;
    let mut hinted = false;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut write = |chunk: &[f32], frames: &mut u64, peak: &mut f32| -> Result<(), String> {
        for &s in chunk {
            wav.write_sample(s).map_err(|e| format!("write {out}: {e}"))?;
            *peak = peak.max(s.abs());
        }
        *frames += (chunk.len() / channels as usize) as u64;
        Ok(())
    };
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => write(&chunk, &mut frames, &mut window_peak)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if last_draw.elapsed() >= Duration::from_millis(100) {
            heard |= window_peak > 0.0;
            let secs = frames as f64 / rate;
            let level = if window_peak > 0.0 { format!("{:6.1} dBFS", 20.0 * window_peak.log10()) } else { "  silent  ".into() };
            eprint!("\r\x1b[K  \x1b[31m●\x1b[0m REC {}   peak {level}   (Enter to stop)", clock(secs));
            let _ = std::io::stderr().flush();
            if !heard && !hinted && started.elapsed().as_secs_f64() > SILENCE_HINT_S {
                hinted = true;
                if frames == 0 {
                    eprintln!("\n  no audio is arriving from the tap — is this macOS 14.2 or later?");
                } else {
                    eprintln!(
                        "\n  only silence so far. If something is playing, allow your terminal app under System Settings ›\n  Privacy & Security › Screen & System Audio Recording (\"System Audio Recording Only\"), then run again."
                    );
                }
            }
            window_peak = 0.0;
            last_draw = Instant::now();
        }
    }

    // 5. Stop, drain what's in flight, finish the file.
    drop(cap);
    while let Ok(chunk) = rx.try_recv() {
        write(&chunk, &mut frames, &mut window_peak)?;
    }
    drop(write);
    wav.finalize().map_err(|e| format!("finish {out}: {e}"))?;
    let lost = dropped.load(Ordering::Relaxed);
    eprintln!("\r\x1b[KSaved {} of audio to {out}", clock(frames as f64 / rate));
    if lost > 0 {
        eprintln!("warning: {lost} buffers were dropped (the writer fell behind)");
    }
    Ok(())
}

fn clock(secs: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    format!("{m}:{:04.1}", secs - m as f64 * 60.0)
}

fn key(k: &CStr) -> Retained<NSString> {
    NSString::from_str(k.to_str().expect("Core Audio keys are ASCII"))
}

fn any<T: objc2::Message>(obj: &T) -> &AnyObject {
    // Every Objective-C object is an AnyObject.
    unsafe { &*(obj as *const T as *const AnyObject) }
}

/// Reads a fixed-size global property of a Core Audio object.
unsafe fn get_property<T: Copy>(object: AudioObjectID, selector: u32) -> Result<T, i32> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = std::mem::size_of::<T>() as u32;
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let st = AudioObjectGetPropertyData(
        object,
        NonNull::from(&address),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new_unchecked(value.as_mut_ptr() as *mut c_void),
    );
    if st == 0 {
        Ok(value.assume_init())
    } else {
        Err(st)
    }
}

fn check(st: i32, what: &str) -> Result<(), String> {
    if st == 0 {
        Ok(())
    } else {
        Err(format!("could not {what} ({})", status(st)))
    }
}

/// An OSStatus as its four-char code when it is one (e.g. 'who?'), else the number.
fn status(st: i32) -> String {
    let b = st.to_be_bytes();
    if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        format!("OSStatus '{}'", String::from_utf8_lossy(&b))
    } else {
        format!("OSStatus {st}")
    }
}
