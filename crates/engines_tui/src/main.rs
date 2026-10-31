//! `engines-tui` — interactive live explorer for the generative drone engine.
//!
//! A desktop dev tool (in the spirit of `engines-play`) that wraps `engines_core` via its public API
//! only — it makes **no changes** to the portable/mobile core. It plays a live session and
//! shows realtime views built from the engine's telemetry + per-loop record stream:
//!   1. a tuneable-param table (operating-point base values; drifted-this-loop param first),
//!   2. a loop log (what the controller moved each loop, Orrery chord, song block, peak) beside
//!      the live control / constraint / override panel,
//!
//! plus a per-instrument screen (spectrograms + oscilloscope, ADR 0024) and live-edit authoring.
//!
//! Run from the repo root so the Setup screen can find `lab/tests/fixtures/*.json{,5}` + `sandbox/*.json{,5}`:
//!   cargo run --release -p engines_tui [-- --doc <path> --optimize drift|hold [--orrery]]

#[cfg(test)]
mod test_support;
mod app;
mod engine_link;
mod setup;
mod ui;
mod viz;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyEventKind};

use app::{App, Args, Screen};

fn main() -> ExitCode {
    let args = parse_args(); // clap exits itself on --help / bad flags

    // The UI renders to stdout; send stderr to a log so any session/core (or audio/MIDI driver)
    // warning printed to stderr can't corrupt the alternate screen.
    let log_path = std::env::temp_dir().join("engines-tui.log");
    #[cfg(unix)]
    let redirect = StderrRedirect::to_file(&log_path);

    let mut terminal = ratatui::init(); // raw mode + alt screen; installs a restoring panic hook
    let res = run(&mut terminal, args);
    ratatui::restore();

    #[cfg(unix)]
    {
        drop(redirect); // restore stderr to the real terminal before printing
        if std::fs::metadata(&log_path).map(|m| m.len() > 0).unwrap_or(false) {
            eprintln!("(diagnostic output logged to {})", log_path.display());
        }
    }

    if let Err(e) = res {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Redirects the process's stderr (fd 2) to a file for the guard's lifetime, restoring it on
/// drop. Anything written to fd 2 meanwhile — core/session warnings, audio or MIDI driver
/// chatter — lands in the log rather than on the TUI's alternate screen.
#[cfg(unix)]
struct StderrRedirect {
    saved: i32,
}

#[cfg(unix)]
impl StderrRedirect {
    fn to_file(path: &std::path::Path) -> Option<Self> {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path).ok()?;
        // SAFETY: dup/dup2 operate on this process's own fds; `saved` is closed on Drop.
        let saved = unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return None;
            }
            if libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) < 0 {
                libc::close(saved);
                return None;
            }
            saved
        };
        Some(StderrRedirect { saved })
    }
}

#[cfg(unix)]
impl Drop for StderrRedirect {
    fn drop(&mut self) {
        // SAFETY: restore the saved stderr onto fd 2 and release the dup.
        unsafe {
            libc::dup2(self.saved, libc::STDERR_FILENO);
            libc::close(self.saved);
        }
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, args: Args) -> std::io::Result<()> {
    let mut app = App::new(args);
    let mut last_tick = Instant::now();

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        // The instrument screen redraws + reveals viz hops at 25 Hz (40 ms) so the spectrogram and
        // oscilloscope animate smoothly (ADR 0024); every other screen is fine at 10 Hz.
        let tick = if app.screen == Screen::Instrument {
            Duration::from_millis(40)
        } else {
            Duration::from_millis(100)
        };

        // Block for input up to the next tick; on timeout, fold in any new loop records/telemetry.
        let timeout = tick.checked_sub(last_tick.elapsed()).unwrap_or(Duration::ZERO);
        if event::poll(timeout)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    app.on_key(k);
                }
            }
        }
        if last_tick.elapsed() >= tick {
            app.tick();
            last_tick = Instant::now();
        }
    }
    Ok(())
}

/// CLI surface (the clap arg-parser crate). All flags are optional
/// preselects — the Setup screen still confirms before starting.
#[derive(clap::Parser, Debug)]
#[command(name = "engines-tui", version, about = "Interactive live explorer for the generative drone engine (Ratatui). \
Run from the repo root so the Setup screen can list lab/tests/fixtures/*.json{,5} + sandbox/*.json{,5}. \
Use a --release build: a debug render is slower than real time and the audio underruns.", after_help = "\
NOTES:
    --output-device (ADR 0027) REDIRECTS all audio: nothing plays locally; the device's
    consumer (a DAW on a loopback like \"BlackHole 16ch\") is the monitor. Opens at the
    device's full channel count — pair 1/2 = the mix, each `\"out_channel\": N` instrument
    gets pair (N, N+1). Must differ from --return-device; the DAW-return monitor mix turns
    off when redirected. Setup shows an \"Output\" selector either way.

    --return-device (ADR 0026 phase D; implies --midi) captures a loopback input carrying
    the DAW's output and monitor-mixes it live (needs an `external_return` bus in the doc;
    ignored when redirected). Setup shows a \"Return\" selector (only with --midi).

KEYS — Setup: Tab/←/→ choose, Enter start, Esc quit. Live: 1/2 or Tab switch views,
↑/↓ select, Enter (Params) inspect the instrument, o optimize, l live-edit, m Orrery,
c/v constraint/override, i instrument screen, q quit.")]
struct Cli {
    /// Preselect a loop document (JSON/JSON5, schema v0.9)
    #[arg(long)]
    doc: Option<String>,
    /// Optimization axis preselect: drift (default) | hold
    #[arg(long)]
    optimize: Option<String>,
    /// Enable generative Orrery harmony (movement axis; combines with any --optimize)
    #[arg(long)]
    orrery: bool,
    /// Write the live objectives doc here + WATCH it: edit + save to re-steer while playing
    #[arg(long)]
    objectives: Option<String>,
    /// Per-loop drift step δ, normalized 0..1 (default: the doc's max_change, else 0.05)
    #[arg(long)]
    max_change: Option<f32>,
    /// Dither RNG seed (default 24301)
    #[arg(long)]
    seed: Option<u64>,
    /// MIDI-out (ADR 0026): deliver `type:"midi"` instruments' Note-On/Off + CC to a
    /// virtual CoreMIDI source, clocked to playback (Note-Offs + CC 123 flush on quit)
    #[arg(long)]
    midi: bool,
    /// Virtual MIDI port name (implies --midi). Default: "Engines Out"
    #[arg(long)]
    midi_port: Option<String>,
    /// DAW-audio-return loopback input, e.g. "BlackHole 2ch" (implies --midi; see NOTES)
    #[arg(long)]
    return_device: Option<String>,
    /// Output redirect (ADR 0027): stream ONLY to this device (see NOTES)
    #[arg(long)]
    output_device: Option<String>,
    /// Midi drift (implies --midi): `type:"midi"` params join drift's candidates
    #[arg(long)]
    dither_midi: bool,
}

fn parse_args() -> Args {
    use clap::Parser;
    let c = Cli::parse();
    // Implications (same semantics as engines-play): a named port, a DAW return, or midi dither
    // each only matter when MIDI-out delivers — they imply --midi.
    let midi = c.midi || c.midi_port.is_some() || c.return_device.is_some() || c.dither_midi;
    Args {
        doc: c.doc,
        optimize: c.optimize,
        orrery: c.orrery,
        objectives: c.objectives,
        max_change: c.max_change,
        seed: c.seed,
        midi,
        midi_port: c.midi_port,
        return_device: c.return_device,
        output_device: c.output_device,
        dither_midi: c.dither_midi,
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_is_wellformed_and_implications_hold() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        for extra in [["--midi-port", "X"], ["--return-device", "BlackHole 2ch"]] {
            let argv: Vec<&str> = ["engines-tui"].into_iter().chain(extra).collect();
            let c = Cli::parse_from(argv);
            let midi = c.midi || c.midi_port.is_some() || c.return_device.is_some() || c.dither_midi;
            assert!(midi, "{extra:?} must imply --midi");
        }
        let c = Cli::parse_from(["engines-tui", "--dither-midi"]);
        assert!(c.dither_midi && (c.midi || c.dither_midi));
    }
}
