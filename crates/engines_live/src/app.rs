//! `engines-live` — play a piece live and apply every save of its doc (ADR 0034, 0035).
//!
//! ```text
//! engines-live <slug> [--output-device NAME] [--optimize hold|drift|live_edit] [--orrery] [--ring-ms 170] [--block 1024]
//! engines-live <slug> --headless [--fast] [--max-loops N]
//! engines-live <slug> --stdio [--headless]          # the JSON-lines protocol (the VS Code extension)
//! engines-live <slug> --loop-block K | --play-from K  # song audition from the terminal
//! engines-live <slug> --output-device "BlackHole 64ch" --route-sources [MAP]  # one pair per source (ADR 0044)
//! engines-live --list-devices [--json]                # the output devices (name, channels, rate)
//! ```
//! Watches `pieces/<slug>/<slug>.json5` (polled on every main-loop pass — 20 ms, 5 ms with
//! `--fast` — one `metadata()` call each; applied once quiet for 300 ms); a save with errors is
//! refused and the last good revision keeps playing; `.live/pause` holds saves back.
//! Every accepted save is snapshotted into `.live/history/` and applied **mid-loop**: the session
//! renders 1024-frame blocks into a ~170 ms ring and crossfades the edit in where it lands.
//! `.live/status.json` reports the revision playing (and where it landed), the loop position, the
//! Orrery chord, per-instrument meters, the peak and the limiter. The device output runs through a
//! live-only −1 dBFS lookahead limiter (never in renders).
//!
//! This is the player as a library: the free `engines-live` runs [`run`] with no note hook;
//! Engines Studio's player runs it with the MIDI mirror's.

use clap::Parser;
use engines_core::routing;
use engines_core::piece::Piece;
use engines_core::playback;
use crate::hooks::NoteHook;
use crate::limiter::{Limiter, LimiterMeter};
use crate::player::{song_block_error, song_blocks, LiveOptions, LivePlayer, Mode};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "engines-live", about = "Play a piece live; every save of its doc is checked, snapshotted and applied mid-loop")]
struct Args {
    /// The piece (`pieces/<SLUG>/`) or the path of its doc.
    #[arg(required_unless_present = "list_devices")]
    piece: Option<String>,
    /// List the output devices (name · channels · sample rate) and exit.
    #[arg(long)]
    list_devices: bool,
    /// With --list-devices: as a JSON array (the VS Code output picker reads it).
    #[arg(long, requires = "list_devices")]
    json: bool,
    /// Output device (default: the system output).
    #[arg(long)]
    output_device: Option<String>,
    /// Override the doc's timbre axis: hold | drift | live_edit.
    #[arg(long, value_parser = ["hold", "drift", "live_edit"])]
    optimize: Option<String>,
    /// Switch the Orrery on (keeping the doc's movement config).
    #[arg(long)]
    orrery: bool,
    /// Milliseconds of audio rendered ahead (most of the edit latency; lower = snappier, riskier).
    #[arg(long, default_value_t = 170.0)]
    ring_ms: f64,
    /// Render block in frames (how often a pending edit is checked).
    #[arg(long, default_value_t = 1024)]
    block: usize,
    #[arg(long, default_value_t = 0x5EED)]
    seed: u64,
    /// No audio device: drain the session at real time (or as fast as possible with --fast).
    #[arg(long)]
    headless: bool,
    /// With --headless: don't pace to real time (tests).
    #[arg(long, requires = "headless")]
    fast: bool,
    /// Stop after N loops.
    #[arg(long)]
    max_loops: Option<usize>,
    /// Turn the protection limiter off (not recommended while editing).
    #[arg(long)]
    no_limiter: bool,
    /// Speak the JSON-lines protocol on stdin/stdout (contracts/engines_live_protocol_v1.md) — how the
    /// VS Code extension drives it.
    #[arg(long)]
    stdio: bool,
    /// Song audition: loop block K of the doc's `song` as a plain loop (edits re-materialize it).
    #[arg(long, value_name = "K", conflicts_with = "play_from")]
    loop_block: Option<usize>,
    /// Song audition: play the doc's `song` from block K to its end (edits wait until it stops).
    #[arg(long, value_name = "K")]
    play_from: Option<usize>,
    /// Route every audio source to its own stereo pair of the output device (ADR 0044) — the Live
    /// set `engines-als` builds listens there. MAP defaults to `pieces/<slug>/ableton/<slug>.routing.json`;
    /// without it the doc's sources play in order from 1/2. Needs a multichannel --output-device
    /// (e.g. "BlackHole 64ch") or --headless.
    #[arg(long, value_name = "MAP", num_args = 0..=1, default_missing_value = "")]
    route_sources: Option<String>,
}

/// What a companion player knows when it builds its [`NoteHook`].
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// `--headless`: no audio device.
    pub headless: bool,
}

static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_sigint() {
    #[cfg(unix)]
    // SAFETY: the handler only stores to an atomic (async-signal-safe).
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_sigint as *const () as libc::sighandler_t);
    }
}

fn resolve(arg: &str) -> Result<Piece, String> {
    let p = Path::new(arg);
    if arg.ends_with(".json5") || arg.ends_with(".json") {
        return Piece::of_doc(p).ok_or_else(|| format!("{arg} isn't a piece doc (pieces/<slug>/<slug>.json5) — use engines-play for other docs"));
    }
    Piece::open(Path::new(""), arg).map_err(|e| e.to_string())
}

/// Run `engines-live` with `argv` (the program name first), the note hook `make_hook` builds (none for
/// the free player).
pub fn run<I: IntoIterator<Item = OsString>>(argv: I, make_hook: &dyn Fn(&Launch) -> Option<Box<dyn NoteHook>>) -> ExitCode {
    let args = Args::parse_from(argv);
    if args.list_devices {
        let devices = playback::output_device_infos();
        if args.json {
            println!("{}", serde_json::to_string(&devices).expect("serializable"));
        } else {
            for d in &devices {
                println!("{} · {} ch · {} Hz{}", d.name, d.channels, d.sample_rate as u32, if d.default { " (default)" } else { "" });
            }
        }
        return ExitCode::SUCCESS;
    }
    let piece = match resolve(args.piece.as_deref().unwrap_or_default()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("engines-live: {e}");
            return ExitCode::from(2);
        }
    };
    // A song block from the command line must exist — refused like the protocol's `song` command
    // (not quietly played as the plain loop). A doc that doesn't load is reported by the start below.
    let block_flag = args.loop_block.map(|k| ("--loop-block", k)).or(args.play_from.map(|k| ("--play-from", k)));
    if let Some((flag, k)) = block_flag {
        if let Some(blocks) = song_blocks(&piece.doc_path()).filter(|&n| k >= n) {
            let e = song_block_error(flag, blocks);
            if args.stdio {
                println!("{}", serde_json::json!({ "type": "error", "message": e }));
            }
            eprintln!("engines-live: {e}");
            return ExitCode::from(2);
        }
    }
    let route = args.route_sources.as_ref().map(|p| if p.is_empty() { routing::path(&piece) } else { PathBuf::from(p) });
    if route.is_some() && args.output_device.is_none() && !args.headless {
        eprintln!("engines-live: --route-sources needs a multichannel --output-device (e.g. \"BlackHole 64ch\") or --headless");
        return ExitCode::from(2);
    }
    install_sigint();
    let opts = |sample_rate: f64, out_channels: usize, device: Option<String>| LiveOptions {
        sample_rate,
        out_channels,
        limiter: !args.no_limiter,
        device,
        ring_ms: args.ring_ms,
        block: args.block,
        seed: args.seed,
        optimize: args.optimize.clone().unwrap_or_default(),
        orrery: args.orrery,
        max_loops: args.max_loops,
        start_mode: match (args.loop_block, args.play_from) {
            (Some(k), _) => Mode::Block(k),
            (None, Some(k)) => Mode::Song(k),
            _ => Mode::Loop,
        },
        route_sources: route.clone(),
        ..Default::default()
    };
    let (mut backend, sr, ch) = if args.headless {
        // Headless routing takes the map's width (a 16-wide default before there is one).
        let ch = route.as_deref().map_or(2, |p| routing::read(p).ok().flatten().map_or(16, |m| m.channels));
        (Backend::Headless { fast: args.fast, pulled: Arc::default(), t0: Instant::now(), epoch: 0, buf: vec![0.0; 8192] }, 48_000.0, ch)
    } else {
        match playback::open_output(args.output_device.as_deref()) {
            Ok(out) => {
                let (sr, ch) = (out.sample_rate, out.out_channels);
                if route.is_some() && sr != 48_000.0 {
                    eprintln!("engines-live: note — \"{}\" runs at {} Hz; renders are 48 kHz, so set it (and Live) to 48 kHz in Audio MIDI Setup", out.name, sr as u32);
                }
                let dev = Device {
                    out,
                    stream: None,
                    epoch: 0,
                    meter: LimiterMeter::new(),
                    limiter_on: Arc::new(AtomicBool::new(!args.no_limiter)),
                    frames_played: Arc::new(AtomicU64::new(0)),
                    underruns: Underruns::default(),
                };
                (Backend::Device(Box::new(dev)), sr, ch)
            }
            Err(e) => {
                eprintln!("engines-live: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    let device = match &backend {
        Backend::Device(d) => Some(d.out.name.clone()),
        Backend::Headless { .. } => None,
    };
    let mut player = match LivePlayer::start_with(piece, &opts(sr, ch, device), make_hook(&Launch { headless: args.headless })) {
        Ok(p) => p,
        Err(e) => {
            if args.stdio {
                println!("{}", serde_json::json!({ "type": "error", "message": e }));
            }
            eprintln!("engines-live: {e}");
            return ExitCode::FAILURE;
        }
    };
    match &backend {
        Backend::Device(d) => eprintln!(
            "engines-live: {} r{} → \"{}\" @ {} Hz · edit {} and save · status {}{}",
            player.piece.slug,
            player.status.rev,
            d.out.name,
            d.out.sample_rate as u32,
            player.piece.doc_path().display(),
            player.piece.live_dir().join("status.json").display(),
            if args.stdio { " · protocol on stdio" } else { " · Ctrl-C to stop" }
        ),
        Backend::Headless { fast, .. } => eprintln!("engines-live: {} r{} headless{}{}", player.piece.slug, player.status.rev, if *fast { " (fast)" } else { "" }, if args.stdio { " · protocol on stdio" } else { "" }),
    }

    // `--stdio`: commands arrive line by line from a reader thread (EOF = quit). A line that isn't
    // UTF-8 is an `error` event, not the end of the session.
    let commands = args.stdio.then(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Option<Result<String, String>>>();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match stdin.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break, // EOF, or the pipe broke
                    Ok(_) => {}
                }
                let line = match std::str::from_utf8(&buf) {
                    Ok(l) if l.trim().is_empty() => continue,
                    Ok(l) => Ok(l.trim_end_matches(['\n', '\r']).to_string()),
                    Err(e) => Err(format!("not UTF-8: {e}")),
                };
                if tx.send(Some(line)).is_err() {
                    return;
                }
            }
            let _ = tx.send(None);
        });
        rx
    });

    // `output` switches the device under the running player (ADR 0044 D7) — by default to the piece's
    // own routing map when it routes.
    let route_map = route.clone().unwrap_or_else(|| routing::path(&player.piece));
    let (mut last_applied, mut last_rejected) = (player.status.applied_rev, false);
    let mut running = true;
    while running && !STOP.load(Ordering::Relaxed) {
        if let Some(rx) = commands.as_ref() {
            while let Ok(msg) = rx.try_recv() {
                let Some(line) = msg else {
                    running = false; // stdin closed
                    break;
                };
                match line.and_then(|l| serde_json::from_str::<serde_json::Value>(&l).map_err(|e| format!("not JSON: {e}"))) {
                    Ok(cmd) if cmd.get("type").and_then(serde_json::Value::as_str) == Some("output") => switch_output(&mut backend, &mut player, &cmd, &route_map),
                    Ok(cmd) => running &= player.handle(&cmd),
                    Err(message) => player.events.push(serde_json::json!({ "type": "error", "message": message })),
                }
            }
        }
        backend.service(&mut player);
        player.tick(Instant::now(), Duration::from_millis(250));
        if args.stdio {
            emit(&mut player);
        } else {
            // No host to hear events: errors (an output device that won't start) go to the console.
            for e in player.events.drain(..) {
                if e["type"] == "error" {
                    eprintln!("engines-live: {}", e["message"].as_str().unwrap_or_default());
                }
            }
            report(&player, &mut last_applied, &mut last_rejected);
            if player.status.state == "finished" {
                break;
            }
        }
        let fast = matches!(backend, Backend::Headless { fast: true, .. });
        std::thread::sleep(Duration::from_millis(if fast { 5 } else { 20 }));
    }
    backend.release();
    player.stop();
    if args.stdio {
        emit(&mut player);
    }
    ExitCode::SUCCESS
}

/// The `output` command (ADR 0044 D7): open `device` (absent/empty: the system output) and play there,
/// each source on its own pair when `route_sources`. Anything that fails — no such device, a routing
/// the device can't carry — is an `error` and the old output keeps playing.
fn switch_output(backend: &mut Backend, player: &mut LivePlayer, cmd: &serde_json::Value, route_map: &Path) {
    let mark = player.events.len();
    let fail = |player: &mut LivePlayer, m: String| player.events.push(serde_json::json!({ "type": "error", "message": format!("output: {m}") }));
    'switch: {
        let Backend::Device(d) = backend else {
            fail(player, "a headless player has no device to switch — start it without --headless".into());
            break 'switch;
        };
        let device = cmd.get("device").and_then(serde_json::Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let route = cmd.get("route_sources").and_then(serde_json::Value::as_bool).unwrap_or(false);
        if route && device.is_none() {
            fail(player, "routing each source to its own pair needs a named multichannel device (e.g. \"BlackHole 64ch\"), not the system output".into());
            break 'switch;
        }
        let out = match playback::open_output(device) {
            Ok(out) => out,
            Err(e) => {
                fail(player, format!("{e} — still playing on \"{}\"", d.out.name));
                break 'switch;
            }
        };
        let route = route.then(|| route_map.to_path_buf());
        if let Err(e) = player.check_output(out.out_channels, route.as_deref()) {
            fail(player, format!("\"{}\" can't carry the routing ({e}) — still playing on \"{}\"", out.name, d.out.name));
            break 'switch;
        }
        if route.is_some() && out.sample_rate != 48_000.0 {
            eprintln!("engines-live: note — \"{}\" runs at {} Hz; renders are 48 kHz, so set it (and Live) to 48 kHz in Audio MIDI Setup", out.name, out.sample_rate as u32);
        }
        eprintln!("engines-live: output → \"{}\" ({} ch @ {} Hz{})", out.name, out.out_channels, out.sample_rate as u32, if route.is_some() { ", each source on its own pair" } else { "" });
        d.retire_stream(); // the next session (now, or at the next play) attaches to the new device
        let (sr, ch, name) = (out.sample_rate, out.out_channels, out.name.clone());
        d.out = out;
        player.set_output(sr, ch, Some(name), route);
    }
    if let Some(id) = cmd.get("id").filter(|v| !v.is_null()) {
        for e in &mut player.events[mark..] {
            e["id"] = id.clone();
        }
    }
}

/// Print and flush pending protocol events (one JSON object per line).
fn emit(player: &mut LivePlayer) {
    let mut out = std::io::stdout().lock();
    for e in player.events.drain(..) {
        let _ = writeln!(out, "{e}");
    }
    let _ = out.flush();
}

/// The device output (cpal) — re-attached whenever the player starts a new session.
struct Device {
    out: playback::OpenOutput,
    stream: Option<playback::OutputStream>,
    epoch: u64,
    meter: Arc<LimiterMeter>,
    limiter_on: Arc<AtomicBool>,
    frames_played: Arc<AtomicU64>,
    underruns: Underruns,
}

impl Device {
    /// Let the stream go (its underruns stay in the running total).
    fn retire_stream(&mut self) {
        self.stream = None;
        self.underruns.retire();
    }
}

/// `underrun.count` — a running total for the process: each cpal stream counts its own, and a new
/// one starts at every `play` / `restart`.
#[derive(Debug, Default)]
struct Underruns {
    /// The streams already let go.
    retired: u64,
    /// The current stream's count as last noted.
    current: u64,
}

impl Underruns {
    fn note(&mut self, stream_count: u64) {
        self.current = stream_count;
    }
    fn retire(&mut self) {
        self.retired += std::mem::take(&mut self.current);
    }
    fn total(&self) -> u64 {
        self.retired + self.current
    }
}

/// Where the session's audio goes.
enum Backend {
    Device(Box<Device>),
    /// No device: drain at real time (or as fast as possible with `--fast`). `pulled` counts the
    /// session's samples drained — the MIDI mirror's clock (ADR 0048).
    Headless { fast: bool, pulled: Arc<AtomicU64>, t0: Instant, epoch: u64, buf: Vec<f32> },
}

impl Backend {
    /// Keep the audio flowing: (re)attach the device stream to a new session, or drain headless.
    fn service(&mut self, player: &mut LivePlayer) {
        match self {
            Backend::Device(d) => {
                if player.epoch != d.epoch {
                    d.epoch = player.epoch;
                    d.retire_stream();
                    // The ring's frame width must be the stream's: a mismatch would scramble the
                    // channels (e.g. a routing narrower than the device), so refuse to play it.
                    let width = player.session_mut().map(|s| s.channels() as usize);
                    if width.is_some_and(|w| w != d.out.out_channels) {
                        let message = format!("output: the session streams {} channels but \"{}\" has {}", width.unwrap_or(0), d.out.name, d.out.out_channels);
                        player.events.push(serde_json::json!({ "type": "error", "message": message }));
                    } else if let Some(cons) = player.session_mut().and_then(|s| s.take_consumer()) {
                        let mut lim = Limiter::new(d.out.sample_rate, d.out.out_channels, -1.0, 1.5, 100.0, d.meter.clone());
                        let on = d.limiter_on.clone();
                        let post: playback::OutputPost = Box::new(move |data: &mut [f32], _ch: usize| {
                            if on.load(Ordering::Relaxed) {
                                lim.process(data);
                            }
                        });
                        match playback::start_output_stream_with(&d.out, cons, None, &d.frames_played, false, Some(post)) {
                            Ok(s) => {
                                // The MIDI mirror follows this session's own audio (ADR 0048).
                                player.attach_note_clock(s.ring_samples_consumed.clone(), d.out.out_channels as u64);
                                d.stream = Some(s);
                            }
                            Err(e) => player.events.push(serde_json::json!({ "type": "error", "message": format!("output: {e}") })),
                        }
                    }
                }
                match player.session_mut().map(|s| s.frames_produced()) {
                    None => d.retire_stream(), // stopped
                    // Finished: the worker renders no more, so an empty ring is the end, not an
                    // underrun — the count stays as it was, and once every rendered frame has reached
                    // the device the stream (silence from here on) is let go.
                    Some(produced) if player.status.state == "finished" => {
                        let width = d.out.out_channels.max(1) as u64;
                        if d.stream.as_ref().is_some_and(|s| s.ring_samples_consumed.load(Ordering::Acquire) / width >= produced) {
                            d.retire_stream();
                        }
                    }
                    Some(_) => {
                        if let Some(s) = d.stream.as_ref() {
                            d.underruns.note(s.underruns.load(Ordering::Relaxed));
                        }
                    }
                }
                player.note_underruns(d.underruns.total());
                d.limiter_on.store(player.limiter_on, Ordering::Relaxed);
                player.note_limiter(player.limiter_on.then(|| d.meter.take_gr_db()));
                player.status.nan_muted = Some(d.meter.nan_muted());
            }
            Backend::Headless { fast, pulled, t0, epoch, buf } => {
                if player.epoch != *epoch {
                    *epoch = player.epoch;
                    *pulled = Arc::default();
                    *t0 = Instant::now();
                    if let Some(width) = player.session_mut().map(|s| s.channels() as u64) {
                        player.attach_note_clock(pulled.clone(), width);
                    }
                }
                let rate = 48_000.0; // headless runs at the default rate
                let Some(session) = player.session_mut() else { return };
                // Pace to real time unless --fast: pull what the wall clock says has "played".
                let width = session.channels() as f64;
                let due = if *fast { u64::MAX } else { (t0.elapsed().as_secs_f64() * rate * width) as u64 };
                let mut done = pulled.load(Ordering::Acquire);
                while done < due {
                    let want = ((due - done).min(buf.len() as u64)) as usize;
                    let n = session.pull_audio(&mut buf[..want]);
                    if n == 0 {
                        break;
                    }
                    done += n as u64;
                    pulled.store(done, Ordering::Release);
                }
            }
        }
    }

    fn release(&mut self) {
        if let Backend::Device(d) = self {
            d.retire_stream();
        }
    }
}

/// Console lines for what changed (the status file has everything).
fn report(player: &LivePlayer, last_applied: &mut u64, last_rejected: &mut bool) {
    let s = &player.status;
    if s.applied_rev != *last_applied {
        *last_applied = s.applied_rev;
        let chord = s.chord.as_deref().map(|c| format!(" · {c}")).unwrap_or_default();
        let at = s.applied_at_s.map(|t| format!(" at {t:.2} s")).unwrap_or_default();
        eprintln!("▸ r{} playing from loop {}{at}{chord}", s.applied_rev, s.applied_at_loop.unwrap_or(0));
    }
    let rejected = s.rejected.is_some();
    if rejected && !*last_rejected {
        eprintln!("✗ save refused — still playing r{}:", s.applied_rev);
        for e in &s.rejected.as_ref().unwrap().errors {
            eprintln!("    {e}");
        }
    }
    *last_rejected = rejected;
}

#[cfg(test)]
mod tests {
    use super::Underruns;

    #[test]
    fn underruns_are_a_running_total_across_streams() {
        let mut u = Underruns::default();
        u.note(3);
        u.note(5);
        assert_eq!(u.total(), 5, "a stream's own counter is cumulative");
        u.retire(); // `play` / `restart`: the next stream counts from 0 …
        u.note(0);
        assert_eq!(u.total(), 5, "… but the total doesn't reset");
        u.note(2);
        u.retire();
        u.retire(); // retired twice (stopped, then released): counted once
        assert_eq!(u.total(), 7);
    }
}
