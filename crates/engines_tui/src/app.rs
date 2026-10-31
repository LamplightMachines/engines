//! App state, screen/tab model, and input handling. The run loop in `main` calls
//! [`App::tick`] (~10 Hz, drains the loop-record stream + telemetry) and [`App::on_key`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use engines_core::contract::{objectives_template_json5, parse_constraint, parse_override, OptimizeMode, SessionControl};
use engines_core::controller::dither_candidates_with;
use engines_core::loop_doc::{param_norm, ConstraintBand, LoopDoc};
use engines_core::session::VizFrame;
use engines_core::telemetry::{LoopRecord, Telemetry};

use engines_core::params::units;

/// Most recent loop records kept for the Log view (a bounded ring — flat memory over long runs).
const LOG_CAP: usize = 256;

/// Instrument-screen spectrogram grid (ADR 0024): log-freq rows + rolling column history; the
/// renderer samples these down to the actual terminal area. The oscilloscope keeps `SCOPE_CAP` hops.
const SPECTRO_ROWS: usize = 48;
const SPECTRO_CAP: usize = 300;
const SCOPE_CAP: usize = 240;
/// Max viz hops revealed per tick (ADR 0024): normally ~1 (one 40 ms hop plays per 40 ms tick); the
/// cap only bounds catch-up after a UI hiccup so one tick can't do unbounded FFT work.
const VIZ_REVEAL_MAX: usize = 256;

/// Which structured control field the inline editor is editing (Phase 6d).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Constraint,
    Override,
}

impl EditKind {
    /// Prompt label + a short input-format hint shown in the editor line.
    pub fn prompt(self) -> (&'static str, &'static str) {
        match self {
            EditKind::Constraint => ("constraint", "path min max (native units) | path -"),
            EditKind::Override => ("override", "path value (native units: Hz, s, dB; -inf mutes a gain) | path -"),
        }
    }
}

/// An open inline editor: which control field, plus the text buffer being typed (Phase 6d).
pub struct EditField {
    pub kind: EditKind,
    pub buf: String,
}

/// One rendered row of the loop log (Phase 3): which loop did what, and where in the audio.
pub struct LogEntry {
    pub loop_id: u64,
    /// Audio position (seconds since session start) of the loop's first frame.
    pub at_s: f64,
    pub text: String,
}

use crate::engine_link::EngineLink;

/// CLI preselects (all optional); the Setup screen still confirms before starting.
#[derive(Default)]
pub struct Args {
    pub doc: Option<String>,
    pub optimize: Option<String>,
    pub orrery: bool,
    pub objectives: Option<String>,
    pub max_change: Option<f32>,
    pub seed: Option<u64>,
    /// MIDI-out (ADR 0026): deliver `type:"midi"` instruments' events to a virtual port.
    pub midi: bool,
    pub midi_port: Option<String>,
    /// DAW-audio-return device (ADR 0026 phase D): preselect a loopback input by name. Only
    /// meaningful with `--midi` + an `external_return` bus in the doc.
    pub return_device: Option<String>,
    /// Output redirect (ADR 0027): preselect the device the engine's audio is streamed to
    /// (a DAW records it as a stereo track).
    pub output_device: Option<String>,
    /// Midi drift (ADR 0026): drift also moves `type:"midi"` params. Implies `--midi`.
    pub dither_midi: bool,
}

/// Setup-screen optimize choices (live-edit is entered in-session with `l`).
pub const OPTIMIZE: [&str; 2] = ["drift", "hold"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Setup,
    Live,
    /// Per-instrument live stats: dry + master spectrograms, param list, oscilloscope (ADR 0024).
    Instrument,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Params,
    Log,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetupField {
    Doc,
    Optimize,
    Orrery,
    /// DAW-audio-return device (ADR 0026 phase D). Only present/navigable when `--midi` is on.
    ReturnDevice,
    /// Midi drift (ADR 0026). Only present/navigable when `--midi` is on.
    MidiDither,
    /// Output device (ADR 0027): "(default)" plays locally; anything else REDIRECTS the
    /// engine there (its consumer — a DAW on a loopback — is the monitor; `out_channel`
    /// instruments get their own stereo pairs). Always present.
    Output,
}

pub struct Setup {
    pub docs: Vec<PathBuf>,
    pub doc_idx: usize,
    pub optimize_idx: usize,
    pub orrery: bool,
    pub field: SetupField,
    pub error: Option<String>,
    /// True when `--midi` was passed: gates the return-device selector's presence (a DAW return
    /// pairs with MIDI-out — you drive the DAW over MIDI, it returns audio here).
    pub midi: bool,
    /// Return-device choices: index 0 is `"(off)"`, the rest are capture-device names.
    pub return_devices: Vec<String>,
    pub return_idx: usize,
    /// Midi drift (ADR 0026): drift also moves `type:"midi"` params. Gated on `--midi`.
    pub dither_midi: bool,
    /// Output-device choices: index 0 is `"(default)"`, the rest are output-device names.
    pub output_devices: Vec<String>,
    pub output_idx: usize,
}

pub struct App {
    pub screen: Screen,
    pub tab: Tab,
    pub setup: Setup,

    pub link: Option<EngineLink>,

    /// Tuneable params of the running doc (`dither_candidates`), the rows of the Params view.
    pub params: Vec<String>,
    pub params_state: TableState,

    // Live-run labels for the status line.
    pub doc_label: String,
    pub mode_label: String,
    pub sample_rate: f64,
    pub finished: bool,
    /// Loops rendered so far (one [`LoopRecord`] each; the render runs a ring's worth ahead of play).
    pub loops: u64,
    /// The most recent loop record — its `path`/`delta` give the Params tab's δ column.
    pub last_record: Option<LoopRecord>,

    /// Runtime `--max-change` override; `None` ⇒ `EngineLink::start` falls back to the doc's
    /// `max_change`, then the built-in default (ADR 0025).
    pub max_change: Option<f32>,
    pub seed: u64,
    /// MIDI-out (ADR 0026): `Some(port name)` when `--midi`/`--midi-port` was given.
    pub midi_port: Option<String>,
    pub should_quit: bool,

    /// Watched objectives-doc path (Phase 1) + last-seen (mtime, content) for the echo guard.
    pub objectives: Option<String>,
    /// True when the objectives path was auto-created in the cwd (no `--objectives` given) — shown in
    /// the control panel so the user knows where it lives.
    pub objectives_auto: bool,
    last_obj: Option<(SystemTime, String)>,
    /// Latest read-only telemetry snapshot from the engine.
    pub telemetry: Option<Telemetry>,

    /// The live control plane (Phase 3): edited in-TUI and written back to the objectives doc.
    pub control: SessionControl,
    /// `Some(field)` while an inline editor is open (constraint / override); `None` otherwise.
    pub editing: Option<EditField>,
    /// Last edit parse error (shown in the editor line).
    pub edit_error: Option<String>,
    /// Bounded ring of recent loop records (rendered lines) for the Log view.
    pub log: VecDeque<LogEntry>,

    /// Watched loop-doc path (LiveEdit, ADR 0024) + last-seen (mtime, content) echo guard. The TUI
    /// only ever *reads* this file — the user authors it in their editor, so their comments are safe.
    pub loop_doc_path: Option<PathBuf>,
    last_doc: Option<(SystemTime, String)>,
    /// The currently-loaded loop doc (live-edit diffing + Phase D safe-bounds lookups).
    pub loop_doc: Option<LoopDoc>,
    /// Last loop-doc parse error, shown in the status line during live-edit.
    pub live_edit_error: Option<String>,

    /// Instrument screen (ADR 0024): selected instrument + param row, the two rolling spectrograms
    /// (dry of the selected instrument + master mix), the selected-param oscilloscope ring, and the
    /// latest per-hop effective (normalized) param values.
    pub inst_idx: usize,
    pub inst_param_idx: usize,
    /// Scroll/selection state for the instrument param list, so it follows the cursor like the
    /// Params tab (ADR 0024). Kept in sync with `inst_param_idx`.
    pub inst_params_state: TableState,
    master_spectro: Option<crate::viz::Spectro>,
    dry_spectro: Option<crate::viz::Spectro>,
    pub scope: VecDeque<f32>,
    /// Viz hops awaiting playback (ADR 0024): drained from the engine in bursts (render runs ahead),
    /// revealed into the spectrograms/scope only once their `audio_pos` reaches the play cursor.
    viz_pending: VecDeque<VizFrame>,
    latest_eff: std::collections::BTreeMap<String, f32>,
}

impl App {
    pub fn new(args: Args) -> Self {
        let mut docs = discover_docs();
        let mut doc_idx = 0;
        if let Some(d) = &args.doc {
            let pb = PathBuf::from(d);
            match docs.iter().position(|p| p == &pb) {
                Some(i) => doc_idx = i,
                None => {
                    docs.insert(0, pb);
                    doc_idx = 0;
                }
            }
        }
        let optimize_idx = args
            .optimize
            .as_ref()
            .and_then(|m| OPTIMIZE.iter().position(|x| x == m))
            .unwrap_or(0);

        // Return-device selector (ADR 0026 phase D): only enumerate capture devices when `--midi`
        // is on (a DAW return pairs with MIDI-out). Index 0 is "(off)".
        let mut return_devices = vec!["(off)".to_string()];
        let mut return_idx = 0;
        if args.midi {
            return_devices.extend(EngineLink::list_input_devices());
            if let Some(want) = &args.return_device {
                if let Some(i) = return_devices.iter().position(|d| d == want || d.contains(want.as_str())) {
                    return_idx = i;
                }
            }
        }
        // Output-device selector (ADR 0027): "(default)" plays locally; any other choice
        // redirects the engine's audio to that device (always offered).
        let mut output_devices = vec!["(default)".to_string()];
        output_devices.extend(EngineLink::list_output_devices());
        let output_idx = args
            .output_device
            .as_ref()
            .and_then(|want| output_devices.iter().position(|d| d == want || d.contains(want.as_str())))
            .unwrap_or(0);

        App {
            screen: Screen::Setup,
            tab: Tab::Params,
            setup: Setup {
                docs,
                doc_idx,
                optimize_idx,
                orrery: args.orrery,
                field: SetupField::Doc,
                error: None,
                midi: args.midi,
                return_devices,
                return_idx,
                dither_midi: args.dither_midi,
                output_devices,
                output_idx,
            },
            link: None,
            params: Vec::new(),
            params_state: TableState::default(),
            doc_label: String::new(),
            mode_label: String::new(),
            sample_rate: 0.0,
            finished: false,
            loops: 0,
            last_record: None,
            max_change: args.max_change,
            seed: args.seed.unwrap_or(0x5EED),
            midi_port: if args.midi { Some(args.midi_port.unwrap_or_else(|| "Engines Out".into())) } else { None },
            should_quit: false,
            objectives: args.objectives,
            objectives_auto: false,
            last_obj: None,
            telemetry: None,
            control: SessionControl::default(),
            editing: None,
            edit_error: None,
            log: VecDeque::new(),
            loop_doc_path: None,
            last_doc: None,
            loop_doc: None,
            live_edit_error: None,
            inst_idx: 0,
            inst_param_idx: 0,
            inst_params_state: TableState::default(),
            master_spectro: None,
            dry_spectro: None,
            scope: VecDeque::new(),
            viz_pending: VecDeque::new(),
            latest_eff: std::collections::BTreeMap::new(),
        }
    }

    /// ~10 Hz: drain the loop-record stream into the Log view and refresh the telemetry snapshot.
    pub fn tick(&mut self) {
        let records = self.link.as_mut().map(|l| l.poll()).unwrap_or_default();
        self.ingest_records(records);
        if let Some(l) = &self.link {
            self.telemetry = l.poll_telemetry();
            if l.worker_finished() {
                self.finished = true;
            }
        }
        self.watch_objectives();
        self.watch_loop_doc();
        // Instrument screen (ADR 0024): the engine produces 25 Hz viz hops during render, which runs
        // ahead of playback, so they arrive in bursts. Buffer them and reveal each hop into the
        // spectrograms/scope only once its audio has played (`audio_pos <= frames_played`), so the
        // newest column tracks the **play** cursor — smooth 25 Hz, in sync with what's audible.
        if self.screen == Screen::Instrument {
            let new_frames = self.link.as_mut().map(|l| l.poll_viz()).unwrap_or_default();
            self.viz_pending.extend(new_frames);
            let played = self.link.as_ref().map(|l| l.frames_played()).unwrap_or(0);
            self.reveal_viz(played, VIZ_REVEAL_MAX);
        }
        // Keep a sensible default selection once rows exist.
        if self.params_state.selected().is_none() && !self.params.is_empty() {
            self.params_state.select(Some(0));
        }
    }

    /// Fold freshly-drained loop records into the bounded Log ring (one line per loop), the loop
    /// counter, and the latest-record slot the Params tab reads its δ from.
    fn ingest_records(&mut self, records: Vec<LoopRecord>) {
        let sr = self.sample_rate.max(1.0);
        for r in records {
            self.loops += 1;
            self.log.push_back(log_entry(&r, sr));
            self.last_record = Some(r);
        }
        while self.log.len() > LOG_CAP {
            self.log.pop_front();
        }
    }

    /// Watch the objectives doc for live edits and apply them (Phase 1; echo-guarded by content
    /// equality so our own initial write isn't re-applied). Parse errors are ignored (the control
    /// simply doesn't change).
    fn watch_objectives(&mut self) {
        let Some(path) = self.objectives.clone() else { return };
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        if mtime == self.last_obj.as_ref().map(|(t, _)| *t) {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else { return };
        if !self.last_obj.as_ref().is_some_and(|(_, c)| c == &content) {
            if let Ok(ctrl) = SessionControl::from_json_str(&content) {
                // Keep the TUI's editable model in sync with an external edit (Phase 3), so a later
                // in-TUI edit doesn't clobber it.
                self.control = ctrl.clone();
                self.refresh_mode_label();
                if let Some(l) = &self.link {
                    l.apply_control(ctrl);
                }
            }
        }
        self.last_obj = mtime.map(|t| (t, content));
    }

    /// Watch the loop doc for external edits and apply them to the next loop (LiveEdit only, ADR 0024).
    /// **Read-only**: the TUI never writes the loop doc, so the author's comments are always safe. A
    /// same-id edit swaps in seamlessly; a structural edit crossfades (handled engine-side). Parse
    /// errors are surfaced (`live_edit_error`) but non-fatal — the next save retries.
    fn watch_loop_doc(&mut self) {
        if self.control.optimize != OptimizeMode::LiveEdit {
            return;
        }
        // A song (ADR 0029) owns the doc — the session drops swaps while one plays; say so here
        // instead of pretending an edit took.
        if self.telemetry.as_ref().is_some_and(|t| t.song.is_some()) {
            self.live_edit_error = Some("song playing — LiveEdit swaps are ignored (ADR 0029)".into());
            return;
        }
        let Some(path) = self.loop_doc_path.clone() else { return };
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        if mtime == self.last_doc.as_ref().map(|(t, _)| *t) {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else { return };
        if !self.last_doc.as_ref().is_some_and(|(_, c)| c == &content) {
            match LoopDoc::from_json_str(&content) {
                Ok(doc) => {
                    if let Some(l) = &self.link {
                        l.apply_doc(doc.clone());
                    }
                    // Refresh the Params rows so a structural edit's new params show up
                    // (midi params included iff the session started with midi dither on).
                    self.params = dither_candidates_with(&doc, self.setup.midi && self.setup.dither_midi);
                    // Doc applied — surface soft authoring lints in the same slot (prefixed, so
                    // it reads as advice, not a failed swap): e.g. a sync:"loop" pattern whose
                    // step grid spans >1 loop silently never fires its later steps.
                    self.live_edit_error = doc.lint().first().map(|w| format!("warning: {w}"));
                    self.loop_doc = Some(doc);
                }
                Err(e) => self.live_edit_error = Some(format!("loop doc: {e}")),
            }
        }
        self.last_doc = mtime.map(|t| (t, content));
    }

    /// Song position for the status line (ADR 0029): the block the worker is rendering (from
    /// telemetry) and the play position against the song length (DAC clock — the render runs up to
    /// a ring's worth ahead). `None` outside a song.
    pub fn song_status(&self) -> Option<String> {
        let t = self.telemetry.as_ref()?.song.as_ref()?;
        let played_s = self.link.as_ref().map(|l| l.frames_played()).unwrap_or(0) as f64 / self.sample_rate.max(1.0);
        Some(song_status_line(t, played_s))
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits (raw mode delivers it as a key event, not a signal).
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        // A modal inline editor (constraint / override) captures all keys until Enter
        // (commit) or Esc (cancel) — on the Live *and* Instrument screens (ADR 0024).
        if self.editing.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.editing = None;
                    self.edit_error = None;
                }
                KeyCode::Enter => self.commit_edit(),
                KeyCode::Char(c) => {
                    if let Some(f) = self.editing.as_mut() {
                        f.buf.push(c);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(f) = self.editing.as_mut() {
                        f.buf.pop();
                    }
                }
                _ => {}
            }
            return;
        }
        match self.screen {
            Screen::Setup => self.on_key_setup(key),
            Screen::Live => self.on_key_live(key),
            Screen::Instrument => self.on_key_instrument(key),
        }
    }

    fn on_key_setup(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Enter => self.start(),
            KeyCode::Tab => self.setup.field = next_field(self.setup.field, 1, self.setup.midi),
            KeyCode::BackTab => self.setup.field = next_field(self.setup.field, -1, self.setup.midi),
            KeyCode::Up | KeyCode::Left => self.cycle_setup(-1),
            KeyCode::Down | KeyCode::Right => self.cycle_setup(1),
            _ => {}
        }
    }

    /// Change the focused list selector.
    fn cycle_setup(&mut self, d: isize) {
        match self.setup.field {
            SetupField::Doc => self.setup.doc_idx = wrap(self.setup.doc_idx, self.setup.docs.len(), d),
            SetupField::Optimize => self.setup.optimize_idx = wrap(self.setup.optimize_idx, OPTIMIZE.len(), d),
            SetupField::Orrery => self.setup.orrery = !self.setup.orrery,
            SetupField::ReturnDevice => {
                self.setup.return_idx = wrap(self.setup.return_idx, self.setup.return_devices.len(), d)
            }
            SetupField::MidiDither => self.setup.dither_midi = !self.setup.dither_midi,
            SetupField::Output => {
                self.setup.output_idx = wrap(self.setup.output_idx, self.setup.output_devices.len(), d)
            }
        }
    }

    fn start(&mut self) {
        self.setup.error = None;
        let Some(doc_path) = self.setup.docs.get(self.setup.doc_idx).cloned() else {
            self.setup.error = Some("no loop doc available (pass --doc <path>)".into());
            return;
        };
        let text = match std::fs::read_to_string(&doc_path) {
            Ok(t) => t,
            Err(e) => {
                self.setup.error = Some(format!("read {}: {e}", doc_path.display()));
                return;
            }
        };
        let doc = match LoopDoc::from_json_str(&text) {
            Ok(d) => d,
            Err(e) => {
                self.setup.error = Some(format!("parse {}: {e}", doc_path.display()));
                return;
            }
        };
        // Midi drift (ADR 0026): only effective with `--midi` (without delivery the external
        // synth never hears the moved CCs). The params table shows midi params too then.
        let dither_midi = self.setup.midi && self.setup.dither_midi;
        let params = dither_candidates_with(&doc, dither_midi);
        let optimize = OPTIMIZE[self.setup.optimize_idx].to_string();
        let orrery = self.setup.orrery;
        // DAW-audio-return (ADR 0026 phase D): a device only when `--midi` is on and a real one is
        // picked (index 0 = "(off)"). `EngineLink::start` errors if the doc has no external_return.
        let return_device = if self.setup.midi && self.setup.return_idx > 0 {
            self.setup.return_devices.get(self.setup.return_idx).cloned()
        } else {
            None
        };
        // Output redirect (ADR 0027): index 0 = "(default)" (play locally).
        let output_device = if self.setup.output_idx > 0 {
            self.setup.output_devices.get(self.setup.output_idx).cloned()
        } else {
            None
        };

        // LiveEdit (ADR 0024): remember the source file + parsed doc so the watcher can apply hand
        // edits and Phase D can read safe bounds. The TUI never *writes* this file.
        self.loop_doc_path = Some(doc_path.clone());
        self.loop_doc = Some(doc.clone());

        match EngineLink::start(doc, &optimize, orrery, self.max_change, self.seed, self.midi_port.as_deref(), return_device.as_deref(), output_device.as_deref(), dither_midi) {
            Ok(mut link) => {
                self.sample_rate = link.sample_rate;
                self.control = link.control().clone(); // Phase 3: the live, editable control plane
                // Shell-facing startup notes (stem fold-back, redirect/return status) — a TUI
                // can't print, so they land in the Log view (ADR 0027).
                for text in link.startup_notes.drain(..) {
                    self.log.push_back(LogEntry { loop_id: 0, at_s: 0.0, text });
                }
                self.link = Some(link);
                // If no `--objectives` was given, auto-create one in the cwd so the control plane is
                // always editable + openable (key `0`); the path is shown in the Log/control panel.
                if self.objectives.is_none() {
                    self.objectives = Some(default_objectives_path());
                    self.objectives_auto = true;
                }
                // Phase 1: seed the watched objectives doc with the starting control, rendered as the
                // inline-commented JSON5 template (ADR 0023) so the file carries built-in help.
                if let Some(path) = self.objectives.clone() {
                    if let Some(l) = &self.link {
                        let init = objectives_template_json5(l.control());
                        if std::fs::write(&path, &init).is_ok() {
                            self.last_obj = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).map(|t| (t, init));
                        }
                    }
                }
                self.params = params;
                self.params_state.select(Some(0));
                self.doc_label = doc_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();
                self.mode_label = if orrery { format!("{optimize}+orrery") } else { optimize };
                self.screen = Screen::Live;
            }
            Err(e) => self.setup.error = Some(e),
        }
    }

    /// Apply the live control plane (Phase 3): bump the revision, push it to the engine, and — if a
    /// watched objectives doc is set — write it back (updating the echo guard so our own write isn't
    /// re-applied by `watch_objectives`).
    fn apply_edit(&mut self) {
        self.control.revision += 1;
        if let Some(l) = &self.link {
            l.apply_control(self.control.clone());
        }
        if let Some(path) = self.objectives.clone() {
            // Re-render through the inline-commented JSON5 template (ADR 0023) so the built-in help
            // survives every TUI edit; the echo guard records exactly what we wrote.
            let text = objectives_template_json5(&self.control);
            if std::fs::write(&path, &text).is_ok() {
                self.last_obj = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).map(|t| (t, text));
            }
        }
        self.refresh_mode_label();
    }

    fn refresh_mode_label(&mut self) {
        let opt = match self.control.optimize {
            OptimizeMode::Hold => "hold",
            OptimizeMode::Drift => "drift",
            OptimizeMode::LiveEdit => "live-edit",
        };
        self.mode_label = if self.control.movement.orrery { format!("{opt}+orrery") } else { opt.to_string() };
    }

    /// Toggle the optimize axis drift⇄hold and apply (live toggle, key `o`).
    fn cycle_optimize(&mut self) {
        self.control.optimize = match self.control.optimize {
            OptimizeMode::Hold => OptimizeMode::Drift,
            OptimizeMode::Drift => OptimizeMode::Hold,
            // `o` exits LiveEdit back to Drift (LiveEdit is entered via its own key, Phase B).
            OptimizeMode::LiveEdit => OptimizeMode::Drift,
        };
        self.apply_edit();
    }

    /// Toggle the movement (Orrery) axis and apply (live toggle, key `m`).
    fn toggle_movement(&mut self) {
        self.control.movement.orrery = !self.control.movement.orrery;
        self.apply_edit();
    }

    /// Open the watched objectives doc in the OS default editor for JSON (key `0`, alias `O`).
    /// Best-effort and non-blocking; auto-created in the cwd at session start if none was passed.
    fn open_objectives_file(&self) {
        if let Some(path) = &self.objectives {
            open_in_editor(path);
        }
    }

    /// Open the watched loop doc in `$EDITOR` (key `d`) — the author's main live-edit surface.
    fn open_loop_doc_file(&self) {
        if let Some(path) = &self.loop_doc_path {
            if let Some(p) = path.to_str() {
                open_in_editor(p);
            }
        }
    }

    /// Enter/exit live-edit authoring mode (key `l`, ADR 0024): turns drift off and starts watching
    /// the loop doc; exiting returns to drift.
    fn toggle_live_edit(&mut self) {
        self.control.optimize = if self.control.optimize == OptimizeMode::LiveEdit {
            OptimizeMode::Drift
        } else {
            self.last_doc = None; // re-read the file fresh on (re-)entry
            self.live_edit_error = None;
            OptimizeMode::LiveEdit
        };
        self.apply_edit();
    }

    // ── Instrument live-stats screen (ADR 0024) ──────────────────────────────

    /// Sorted instrument ids of the running doc (the screen's selectable instruments).
    pub fn instruments(&self) -> Vec<String> {
        self.loop_doc.as_ref().map(|d| d.instruments.keys().cloned().collect()).unwrap_or_default()
    }

    /// The currently-selected instrument id.
    pub fn selected_instrument(&self) -> Option<String> {
        self.instruments().get(self.inst_idx).cloned()
    }

    /// Numeric (drift-candidate) params of the selected instrument — the param rows, JSONPaths.
    pub fn inst_params(&self) -> Vec<String> {
        let Some(sel) = self.selected_instrument() else { return Vec::new() };
        let prefix = format!("instruments.{sel}.params.");
        self.params.iter().filter(|p| p.starts_with(&prefix)).cloned().collect()
    }

    /// The selected param row's JSONPath.
    pub fn selected_param(&self) -> Option<String> {
        self.inst_params().get(self.inst_param_idx).cloned()
    }

    /// Effective (normalized) value to plot on the oscilloscope for `path`: the live modulated value
    /// from this hop's snapshot if present, else the authored base — so a static (unmodulated) param
    /// shows a flat line instead of an empty scope (ADR 0024, bug 1). Same resolver the param table
    /// uses for its effective column.
    fn scope_value(&self, eff: &std::collections::BTreeMap<String, f32>, path: &str) -> Option<f32> {
        eff.get(path)
            .copied()
            .or_else(|| self.loop_doc.as_ref().and_then(|d| d.get_param(path)).map(|n| param_norm(path, n)))
    }

    /// Reveal buffered viz hops whose audio has played (`audio_pos <= played`) into the spectrograms,
    /// oscilloscope, and effective-value snapshot — at most `max` per call. Returns the count revealed.
    /// The scope sample and `latest_eff` are written from the **same** revealed hop, so the
    /// oscilloscope's newest sample always matches the param table's effective value (play-cursor
    /// aligned, ADR 0024). A hop with `audio_pos > played` (audio not yet heard) stays buffered.
    fn reveal_viz(&mut self, played: u64, max: usize) -> usize {
        let sel_param = self.selected_param();
        let mut revealed = 0;
        while revealed < max && self.viz_pending.front().is_some_and(|fr| fr.audio_pos <= played) {
            let fr = self.viz_pending.pop_front().unwrap();
            if let Some(s) = self.master_spectro.as_mut() {
                s.push_hop(&fr.master);
            }
            if let Some(s) = self.dry_spectro.as_mut() {
                s.push_hop(&fr.inst_dry);
            }
            if let Some(p) = &sel_param {
                if let Some(v) = self.scope_value(&fr.eff, p) {
                    self.scope.push_back(v);
                }
            }
            self.latest_eff = fr.eff;
            revealed += 1;
        }
        while self.scope.len() > SCOPE_CAP {
            self.scope.pop_front();
        }
        revealed
    }

    pub fn master_spectro(&self) -> Option<&crate::viz::Spectro> {
        self.master_spectro.as_ref()
    }
    pub fn dry_spectro(&self) -> Option<&crate::viz::Spectro> {
        self.dry_spectro.as_ref()
    }
    pub fn latest_eff(&self) -> &std::collections::BTreeMap<String, f32> {
        &self.latest_eff
    }

    /// Base (operating-point) value for `path` in native units, for the Params tab. Normally the
    /// latest rendered loop's operating point (telemetry — where drift has walked the base), falling
    /// back to the loaded loop doc. In LiveEdit the loop doc wins, so a base hand-edited in the doc
    /// shows immediately rather than a ring's worth of render-ahead later (ADR 0024). Returns `None`
    /// only if neither source has the param.
    pub fn param_base_value(&self, path: &str) -> Option<f32> {
        let from_op = || self.telemetry.as_ref().and_then(|t| value_at(&t.operating_point, path));
        let from_doc = || self.loop_doc.as_ref().and_then(|d| d.get_param(path));
        if self.control.optimize == OptimizeMode::LiveEdit {
            from_doc().or_else(from_op)
        } else {
            from_op().or_else(from_doc)
        }
    }

    /// The param the controller stepped in the latest rendered loop (`Telemetry::chosen_path`),
    /// if any.
    pub fn perturbed_now(&self) -> Option<&str> {
        self.telemetry.as_ref().map(|t| t.chosen_path.as_str()).filter(|p| !p.is_empty())
    }

    /// Signed native-unit step applied at `path` in the latest rendered loop, if `path` is the
    /// param moved there (from the latest [`LoopRecord`], matched to the telemetry's loop so the δ
    /// always belongs to the highlighted move). `None` for a categorical move (δ 0) or no move.
    pub fn perturbed_delta(&self, path: &str) -> Option<f32> {
        let t = self.telemetry.as_ref()?;
        let r = self.last_record.as_ref()?;
        (r.loop_id == t.loop_id && r.path == path && t.chosen_path == path && r.delta != 0.0).then_some(r.delta)
    }

    /// Enter the instrument screen: turn on the engine's viz tap for the selected instrument and
    /// allocate the rolling spectrograms (sized to a fixed grid; the renderer samples to the area).
    fn enter_instrument(&mut self) {
        if self.link.is_none() {
            return;
        }
        self.screen = Screen::Instrument;
        self.inst_param_idx = 0;
        self.inst_params_state.select(Some(0));
        self.scope.clear();
        self.viz_pending.clear();
        let sr = self.sample_rate as f32;
        self.master_spectro = Some(crate::viz::Spectro::new(SPECTRO_ROWS, SPECTRO_CAP, sr));
        self.dry_spectro = Some(crate::viz::Spectro::new(SPECTRO_ROWS, SPECTRO_CAP, sr));
        if let Some(l) = &self.link {
            l.set_viz(true);
            l.set_viz_instrument(self.selected_instrument());
        }
    }

    /// Leave the instrument screen back to Live; turn the viz tap off so renders pay nothing.
    fn leave_instrument(&mut self) {
        if let Some(l) = &self.link {
            l.set_viz(false);
        }
        self.master_spectro = None;
        self.dry_spectro = None;
        self.viz_pending.clear();
        self.screen = Screen::Live;
    }

    /// Cycle the selected instrument; re-target the viz tap and reset the dry spectrogram + scope.
    fn select_instrument(&mut self, d: isize) {
        let n = self.instruments().len();
        if n == 0 {
            return;
        }
        self.inst_idx = wrap(self.inst_idx, n, d);
        self.inst_param_idx = 0;
        self.inst_params_state.select(Some(0));
        self.scope.clear();
        if let Some(s) = self.dry_spectro.as_mut() {
            s.clear();
        }
        if let Some(l) = &self.link {
            l.set_viz_instrument(self.selected_instrument());
        }
    }

    fn move_inst_param(&mut self, d: isize) {
        let n = self.inst_params().len();
        if n == 0 {
            return;
        }
        self.inst_param_idx = wrap(self.inst_param_idx, n, d);
        self.inst_params_state.select(Some(self.inst_param_idx));
        self.scope.clear(); // restart the oscilloscope for the newly-selected param
    }

    /// Open the instrument viz screen focused on the param highlighted in the Params tab (Enter on
    /// the Live screen, ADR 0024): if the row is an `instruments.<name>.params.<leaf>` path, select
    /// that instrument and pre-select the param. Non-instrument rows (global/matrix/bus) have no
    /// per-instrument screen, so Enter is a no-op there. `enter_instrument`'s screen switch is gated
    /// on an active engine link; the index targeting set here runs regardless.
    fn open_selected_param_in_instrument(&mut self) {
        let Some(path) = self.selected_params_param() else { return };
        let Some(inst) = path.strip_prefix("instruments.").and_then(|r| r.split('.').next()) else {
            return; // global/matrix/bus param — nothing to inspect on the instrument screen
        };
        let Some(i) = self.instruments().iter().position(|n| n.as_str() == inst) else { return };
        self.inst_idx = i; // set before enter_instrument so the viz tap targets this instrument
        self.enter_instrument(); // resets inst_param_idx + scope, turns the viz tap on
        if let Some(pi) = self.inst_params().iter().position(|p| *p == path) {
            self.inst_param_idx = pi; // pre-select the row the user was on (enter_instrument reset it)
            self.inst_params_state.select(Some(pi));
        }
    }

    fn on_key_instrument(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Char('i') => self.leave_instrument(),
            KeyCode::Up => self.move_inst_param(-1),
            KeyCode::Down => self.move_inst_param(1),
            KeyCode::Left | KeyCode::Char('[') => self.select_instrument(-1),
            KeyCode::Right | KeyCode::Char(']') => self.select_instrument(1),
            KeyCode::Char('c') => self.edit_selected_constraint(),
            _ => {}
        }
    }

    /// Open an inline editor of `kind`, seeded with `buf` (Phase 6d).
    fn begin_edit(&mut self, kind: EditKind, buf: String) {
        self.editing = Some(EditField { kind, buf });
        self.edit_error = None;
    }

    /// Seed + open the constraint editor for the selected instrument param (key `c` on the
    /// instrument screen, ADR 0024) — the user types the `min max` band; commit validates it.
    fn edit_selected_constraint(&mut self) {
        if let Some(path) = self.selected_param() {
            self.begin_edit(EditKind::Constraint, format!("{path} "));
        }
    }

    /// Validate a Tier-3 constraint band against the param's Tier-2 safe range (ADR 0024): each
    /// present bound must sit within the safe range, and min ≤ max. Bounds are in canonical storage
    /// units; error messages render in native units (Hz/s/…) to match what the user typed + sees.
    fn validate_band(&self, path: &str, b: &ConstraintBand) -> Result<(), String> {
        let (lo, hi) = self.loop_doc.as_ref().map(|d| d.safe_range(path)).unwrap_or_else(|| engines_core::loop_doc::param_range(path));
        let nyq = (self.sample_rate as f32 * 0.5).max(1.0);
        let nat = |v: f32| units::fmt_native(path, param_norm(path, v), nyq);
        if let Some(min) = b.min {
            if min < lo || min > hi {
                return Err(format!("min {} outside safe [{}..{}]", nat(min), nat(lo), nat(hi)));
            }
        }
        if let Some(max) = b.max {
            if max < lo || max > hi {
                return Err(format!("max {} outside safe [{}..{}]", nat(max), nat(lo), nat(hi)));
            }
        }
        if let (Some(a), Some(c)) = (b.min, b.max) {
            if a > c {
                return Err(format!("min {} > max {}", nat(a), nat(c)));
            }
        }
        Ok(())
    }

    /// Commit the open inline edit: parse the buffer per its kind, merge into the live control, apply.
    /// A parse error keeps the editor open (with the message) so the user can fix it (Phase 6d).
    fn commit_edit(&mut self) {
        let Some(field) = self.editing.take() else { return };
        let res: Result<(), String> = match field.kind {
            EditKind::Constraint => parse_constraint(&field.buf).and_then(|(path, band)| match band {
                Some(b) => {
                    // The user types native units (Hz/s/ms/st/dB, what the screen shows) — what a
                    // schema v0.9 band stores (ADR 0054).
                    self.validate_band(&path, &b)?; // Tier-3 ⊆ Tier-2 (ADR 0024)
                    self.control.constraints.insert(path, b);
                    Ok(())
                }
                None => {
                    self.control.constraints.remove(&path);
                    Ok(())
                }
            }),
            EditKind::Override => parse_override(&field.buf).map(|(path, val)| match val {
                Some(v) => {
                    self.control.overrides.insert(path, v);
                }
                None => {
                    self.control.overrides.remove(&path);
                }
            }),
        };
        match res {
            Ok(()) => {
                self.edit_error = None;
                self.apply_edit();
            }
            Err(e) => {
                self.edit_error = Some(e);
                self.editing = Some(field);
            }
        }
    }

    fn on_key_live(&mut self, key: KeyEvent) {
        // (The modal editor is captured in `on_key` before dispatch — works on every live screen.)
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab | KeyCode::Char('\t') => self.toggle_tab(),
            KeyCode::Char('1') => self.tab = Tab::Params,
            KeyCode::Char('2') => self.tab = Tab::Log,
            // Phase 3 live control: toggle optimize, toggle Orrery.
            KeyCode::Char('o') => self.cycle_optimize(),
            KeyCode::Char('m') => self.toggle_movement(),
            // Phase 6d: structured constraint (`c`) and override (`v`) editors, same write-back path.
            KeyCode::Char('c') => self.begin_edit(EditKind::Constraint, String::new()),
            KeyCode::Char('v') => self.begin_edit(EditKind::Override, String::new()),
            KeyCode::Char('0') | KeyCode::Char('O') => self.open_objectives_file(),
            // LiveEdit authoring (ADR 0024): `l` toggles live-edit mode, `d` opens the loop doc.
            KeyCode::Char('l') => self.toggle_live_edit(),
            KeyCode::Char('d') => self.open_loop_doc_file(),
            // Instrument live-stats screen (ADR 0024).
            KeyCode::Char('i') => self.enter_instrument(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            // Params: drill into the per-instrument viz screen for the selected instrument param
            // (ADR 0024); non-instrument rows are a no-op.
            KeyCode::Enter if self.tab == Tab::Params => self.open_selected_param_in_instrument(),
            _ => {}
        }
    }

    fn toggle_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Params => Tab::Log,
            Tab::Log => Tab::Params,
        };
    }

    /// The Params-tab rows in display order — drifted-this-loop first, then alphabetical. Both
    /// `ui::draw_params` and the Enter handler read this, so the highlighted row and the param we
    /// act on can never disagree (the table is rendered from exactly this order).
    pub fn params_display_order(&self) -> Vec<String> {
        let perturbed = self.perturbed_now().map(str::to_string);
        let mut order = self.params.clone();
        order.sort_by(|a, b| {
            let pa = perturbed.as_deref() == Some(a.as_str());
            let pb = perturbed.as_deref() == Some(b.as_str());
            pb.cmp(&pa).then(a.as_str().cmp(b.as_str()))
        });
        order
    }

    /// The param JSONPath currently highlighted in the Params tab (display order), if any.
    fn selected_params_param(&self) -> Option<String> {
        let i = self.params_state.selected()?;
        self.params_display_order().get(i).cloned()
    }

    /// Move the active table's selection, clamped to its current row count.
    fn move_selection(&mut self, d: isize) {
        let (state, len) = match self.tab {
            Tab::Params => (&mut self.params_state, self.params.len()),
            Tab::Log => return, // the log auto-scrolls (newest first); no row selection
        };
        if len == 0 {
            state.select(None);
            return;
        }
        let cur = state.selected().unwrap_or(0) as isize;
        let next = (cur + d).rem_euclid(len as isize) as usize;
        state.select(Some(next));
    }
}

/// Cycle the focused Setup field through [`crate::setup::SETUP_FIELDS`] — the one ordered
/// list the renderer also draws from, so focus order and row order can never diverge.
/// (`ReturnDevice`/`MidiDither` are midi-gated there, ADR 0026.)
fn next_field(f: SetupField, d: isize, midi: bool) -> SetupField {
    let order: Vec<SetupField> = crate::setup::visible_fields(midi).map(|s| s.id).collect();
    let i = order.iter().position(|x| *x == f).unwrap_or(0);
    order[wrap(i, order.len(), d)]
}

/// Wrap `i` by `d` within `[0, len)` (len 0 ⇒ 0).
fn wrap(i: usize, len: usize, d: isize) -> usize {
    if len == 0 {
        return 0;
    }
    ((i as isize + d).rem_euclid(len as isize)) as usize
}

/// Numeric value at a dotted JSONPath within a nested doc `Value` (the telemetry's native-unit
/// `operating_point` mirror) — mirrors [`LoopDoc::get_param`] on a raw `Value` (object path, no
/// arrays).
fn value_at(v: &serde_json::Value, path: &str) -> Option<f32> {
    let mut cur = v;
    for p in path.split('.') {
        cur = cur.get(p)?;
    }
    cur.as_f64().map(|f| f as f32)
}

/// A drift step in its param's unit, as the doc writes it (schema v0.9): `+12.5Hz`, `-0.40dB`,
/// `+0.050s`, `+0.031` for a plain 0..1 param.
pub fn fmt_step(path: &str, d: f32) -> String {
    match units::unit_label(path) {
        "0..1" => format!("{d:+.3}"),
        "Hz" => format!("{d:+.1}Hz"),
        "dB" => format!("{d:+.2}dB"),
        "s" => format!("{d:+.3}s"),
        "ms" | "¢" => format!("{d:+.1}{}", units::unit_label(path)),
        "st" => format!("{d:+.2}st"),
        other => format!("{d:+.3}{other}"),
    }
}

/// Render one [`LoopRecord`] for the Log view: the optimize mode, the drift step (native units),
/// the Orrery chord, a categorical flip, the song block/seam, and the loop's master peak in dBFS —
/// e.g. `drift · instruments.poly.params.cutoff_hz +12.5Hz · ♪Bm7 · ♫b2 seam · -14.2 dBFS`.
fn log_entry(r: &LoopRecord, sample_rate: f64) -> LogEntry {
    let mut parts = vec![r.optimize.clone()];
    if !r.path.is_empty() && r.delta != 0.0 {
        parts.push(format!("{} {}", r.path, fmt_step(&r.path, r.delta)));
    }
    if let Some(hm) = &r.harmonic_move {
        parts.push(format!("♪{}", hm.chord));
    }
    if let Some(cm) = &r.categorical_move {
        parts.push(format!("⇄ {} {}→{}", short_path(&cm.path), cm.from, cm.to));
    }
    if let Some(sg) = &r.song {
        parts.push(if sg.seam { format!("♫b{} seam", sg.block + 1) } else { format!("♫b{}", sg.block + 1) });
    }
    parts.push(if r.peak > 0.0 { format!("{:.1} dBFS", 20.0 * r.peak.log10()) } else { "-inf dBFS".to_string() });
    LogEntry { loop_id: r.loop_id, at_s: r.start_frame as f64 / sample_rate.max(1.0), text: parts.join(" · ") }
}

/// Last two dot-segments of a JSONPath (e.g. `instruments.poly.params.cutoff_hz` → `params.cutoff_hz`).
fn short_path(path: &str) -> String {
    let mut it = path.rsplit('.');
    match (it.next(), it.next()) {
        (Some(leaf), Some(parent)) => format!("{parent}.{leaf}"),
        (Some(leaf), None) => leaf.to_string(),
        _ => path.to_string(),
    }
}

/// Open `path` in the OS default app for its type (the user's JSON editor). Best-effort and
/// non-blocking — the TUI is the shell, so spawning here keeps the core I/O-free. Launch failures
/// (no associated app / command missing) are ignored.
fn open_in_editor(path: &str) {
    let _ = open_command().arg(path).spawn();
}

/// Default objectives-doc path when `--objectives` isn't given: `engines-objectives.json5` in the current
/// working directory, absolute so the displayed path and the `0`/`O` opener are unambiguous. JSON5 so
/// the auto-created file carries inline help comments (ADR 0023).
fn default_objectives_path() -> String {
    let name = "engines-objectives.json5";
    std::env::current_dir()
        .map(|d| d.join(name).to_string_lossy().into_owned())
        .unwrap_or_else(|_| name.to_string())
}

#[cfg(target_os = "macos")]
fn open_command() -> std::process::Command {
    // `-t` opens in the default *text* editor (LaunchServices), so it works for `.json5` too — a
    // plain `open file.json5` fails when no app is associated with that extension (the `0` key bug).
    let mut c = std::process::Command::new("open");
    c.arg("-t");
    c
}
#[cfg(target_os = "linux")]
fn open_command() -> std::process::Command {
    std::process::Command::new("xdg-open")
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn open_command() -> std::process::Command {
    let mut c = std::process::Command::new("cmd");
    c.args(["/C", "start", ""]);
    c
}

/// Loop docs to offer on the Setup screen: `*.json` + `*.json5` (ADR 0023) under `lab/tests/fixtures/`
/// then `sandbox/`
/// (run from the repo root). Each group is sorted; sandbox docs come last so personal scratch
/// docs are easy to find. Arbitrary paths can still be supplied with `--doc`.
fn discover_docs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for dir in ["lab/tests/fixtures", "sandbox"] {
        let mut group = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if matches!(p.extension().and_then(|s| s.to_str()), Some("json" | "json5")) {
                    group.push(p);
                }
            }
        }
        group.sort();
        v.extend(group);
    }
    v
}

/// The status-line song segment for telemetry `t` at play position `played_s`. The output stream
/// stays open after a song ends, so the DAC clock keeps counting — the position is clamped to the
/// song length.
fn song_status_line(t: &engines_core::telemetry::SongTelemetry, played_s: f64) -> String {
    let at = if t.tail {
        format!("tail {}/{}", t.loop_in_block + 1, t.block_loops)
    } else {
        format!("{}/{} {} {}/{}", t.block + 1, t.blocks, t.label, t.loop_in_block + 1, t.block_loops)
    };
    let seam = if t.seam { " ⇢seam" } else { "" };
    format!("♫ {at}{seam} · {}/{}", engines_core::song::fmt_hms(played_s.min(t.total_s)), engines_core::song::fmt_hms(t.total_s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engines_core::controller::dither_candidates;

    #[test]
    fn wrap_cycles_both_directions() {
        assert_eq!(wrap(0, 3, -1), 2);
        assert_eq!(wrap(2, 3, 1), 0);
        assert_eq!(wrap(1, 3, 1), 2);
        assert_eq!(wrap(0, 0, 1), 0);
    }

    #[test]
    fn field_focus_order_wraps() {
        // Without --midi, ReturnDevice/MidiDither are not in the cycle; Output always is.
        assert_eq!(next_field(SetupField::Doc, 1, false), SetupField::Optimize);
        assert_eq!(next_field(SetupField::Orrery, 1, false), SetupField::Output);
        assert_eq!(next_field(SetupField::Output, 1, false), SetupField::Doc);
        assert_eq!(next_field(SetupField::Doc, -1, false), SetupField::Output);
    }

    #[test]
    fn midi_fields_only_in_cycle_with_midi() {
        // With --midi: Orrery → ReturnDevice → MidiDither → Output → wraps to Doc.
        assert_eq!(next_field(SetupField::Orrery, 1, true), SetupField::ReturnDevice);
        assert_eq!(next_field(SetupField::ReturnDevice, 1, true), SetupField::MidiDither);
        assert_eq!(next_field(SetupField::MidiDither, 1, true), SetupField::Output);
        assert_eq!(next_field(SetupField::Output, 1, true), SetupField::Doc);
        assert_eq!(next_field(SetupField::Doc, -1, true), SetupField::Output);
    }

    #[test]
    fn default_objectives_path_is_absolute_in_cwd() {
        let p = default_objectives_path();
        assert!(p.ends_with("engines-objectives.json5"), "{p}");
        assert!(std::path::Path::new(&p).is_absolute(), "absolute so the displayed path/opener is unambiguous: {p}");
    }

    #[test]
    fn new_does_not_auto_create_objectives_until_start() {
        // Auto-create happens at start(), not new(), so unit tests + the cwd aren't polluted.
        let app = App::new(Args::default());
        assert!(app.objectives.is_none());
        assert!(!app.objectives_auto);
    }

    // ── Phase 3 — live control edits ─────────────────────────────────────────

    #[test]
    fn live_edit_cycles_optimize_and_bumps_revision() {
        let mut app = App::new(Args::default());
        app.control = SessionControl::default(); // drift, revision 0
        app.cycle_optimize(); // drift → hold
        assert!(matches!(app.control.optimize, OptimizeMode::Hold));
        assert_eq!(app.control.revision, 1, "an edit bumps the revision (the replay timeline)");
        assert_eq!(app.mode_label, "hold");
        app.toggle_movement(); // Orrery on
        assert!(app.control.movement.orrery);
        assert_eq!(app.control.revision, 2);
        assert_eq!(app.mode_label, "hold+orrery");
        app.cycle_optimize(); // hold → drift (a two-way toggle now)
        assert!(matches!(app.control.optimize, OptimizeMode::Drift));
        assert_eq!(app.mode_label, "drift+orrery");
    }

    #[test]
    fn apply_edit_writes_objectives_and_is_echo_guarded() {
        let mut app = App::new(Args::default());
        let path = std::env::temp_dir().join(format!("engines_tui_echo_{}.json", std::process::id()));
        app.objectives = Some(path.to_string_lossy().into_owned());
        app.control = SessionControl::default();
        app.cycle_optimize(); // → apply_edit writes the doc + records the echo guard
        assert!(path.exists(), "apply_edit wrote the objectives doc");
        let written = std::fs::read_to_string(&path).unwrap();
        let (_, recorded) = app.last_obj.clone().expect("echo guard recorded our write");
        assert_eq!(written, recorded, "echo guard holds exactly what we wrote");
        let rev_before = app.control.revision;
        app.watch_objectives(); // our own write must NOT be re-applied
        assert_eq!(app.control.revision, rev_before, "own write is not echoed back");
        let _ = std::fs::remove_file(&path);
    }

    // ── ADR 0024 — live-edit authoring mode ──────────────────────────────────

    #[test]
    fn toggle_live_edit_enters_and_exits() {
        let mut app = App::new(Args::default());
        app.control = SessionControl::default(); // drift
        app.toggle_live_edit();
        assert!(matches!(app.control.optimize, OptimizeMode::LiveEdit));
        assert_eq!(app.mode_label, "live-edit");
        app.toggle_live_edit();
        assert!(matches!(app.control.optimize, OptimizeMode::Drift), "toggles back to drift");
    }

    #[test]
    fn watch_loop_doc_applies_external_edits_only_in_live_edit() {
        let mut app = App::new(Args::default());
        let path = std::env::temp_dir().join(format!("engines_tui_loopdoc_{}.json", std::process::id()));
        let doc_json = crate::test_support::fixture_json();
        std::fs::write(&path, &doc_json).unwrap();
        app.loop_doc_path = Some(path.clone());

        // Outside live-edit the watcher is a no-op (never reads the doc).
        app.control.optimize = OptimizeMode::Drift;
        app.watch_loop_doc();
        assert!(app.loop_doc.is_none(), "watcher must not read the doc outside live-edit");

        // In live-edit it loads + parses the doc and refreshes the param rows.
        app.control.optimize = OptimizeMode::LiveEdit;
        app.watch_loop_doc();
        assert!(app.loop_doc.is_some(), "live-edit watcher loads the doc");
        assert!(app.live_edit_error.is_none());
        assert!(!app.params.is_empty(), "params refreshed from the loaded doc");

        // Unchanged file ⇒ echo guard makes it a no-op.
        let n = app.params.len();
        app.watch_loop_doc();
        assert_eq!(app.params.len(), n);

        // A bad save surfaces a non-fatal error (re-read forced past mtime resolution).
        app.last_doc = None;
        std::fs::write(&path, "{ not valid json5 :::").unwrap();
        app.watch_loop_doc();
        assert!(app.live_edit_error.is_some(), "parse error surfaced, not a crash");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn song_status_and_live_edit_guard() {
        let mut app = App::new(Args::default());
        assert!(app.song_status().is_none(), "no song ⇒ no segment");
        let song = engines_core::telemetry::SongTelemetry {
            block: 1,
            blocks: 4,
            label: "deep".into(),
            loop_in_block: 36,
            block_loops: 540,
            loop_in_song: 236,
            total_loops: 1360,
            tail: false,
            seam: true,
            elapsed_s: 3432.7,
            total_s: 19_781.8,
        };
        app.telemetry = Some(Telemetry { song: Some(song.clone()), ..Default::default() });
        assert_eq!(app.song_status().as_deref(), Some("♫ 2/4 deep 37/540 ⇢seam · 0:00:00/5:29:42"));
        app.telemetry = Some(Telemetry { song: Some(engines_core::telemetry::SongTelemetry { tail: true, loop_in_block: 0, block_loops: 2, seam: false, ..song.clone() }), ..Default::default() });
        assert!(app.song_status().unwrap().starts_with("♫ tail 1/2 ·"));
        // Long after the song ended (the stream stays open), the position stops at the end.
        assert!(song_status_line(&song, 30_000.0).ends_with("· 5:29:42/5:29:42"));

        // LiveEdit while a song plays: the edit is refused visibly, the doc never read.
        let path = std::env::temp_dir().join(format!("engines_tui_song_guard_{}.json", std::process::id()));
        std::fs::write(&path, crate::test_support::fixture_json()).unwrap();
        app.loop_doc_path = Some(path.clone());
        app.control.optimize = OptimizeMode::LiveEdit;
        app.watch_loop_doc();
        assert!(app.loop_doc.is_none());
        assert!(app.live_edit_error.as_deref().is_some_and(|e| e.contains("song playing")));
        let _ = std::fs::remove_file(&path);
    }

    use crate::test_support::fixture_doc;

    #[test]
    fn constraint_validation_enforces_tier2_safe_range() {
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        let (lo, hi) = doc.safe_range("global.master_gain_db");
        app.loop_doc = Some(doc);
        // A band inside the Tier-2 safe range is accepted.
        assert!(app.validate_band("global.master_gain_db", &ConstraintBand { min: Some(lo), max: Some(hi) }).is_ok());
        // Below the safe floor (the master can't be driven to silence) is rejected.
        assert!(app.validate_band("global.master_gain_db", &ConstraintBand { min: Some(lo - 20.0), max: Some(hi) }).is_err());
        // An inverted band is rejected.
        assert!(app.validate_band("global.master_gain_db", &ConstraintBand { min: Some(hi), max: Some(lo) }).is_err());
    }

    #[test]
    fn constraint_commit_rejects_out_of_safe_band_and_keeps_editor_open() {
        let mut app = App::new(Args::default());
        app.loop_doc = Some(fixture_doc());
        app.begin_edit(EditKind::Constraint, "global.master_gain_db -40 -1".to_string());
        app.commit_edit();
        assert!(app.editing.is_some(), "an invalid constraint keeps the editor open");
        assert!(app.edit_error.is_some());
        assert!(!app.control.constraints.contains_key("global.master_gain_db"), "rejected band is not stored");
    }

    #[test]
    fn enter_on_instrument_param_targets_that_instrument_and_preselects_it() {
        // Enter on a Params-tab instrument row jumps to the instrument viz screen for that instrument
        // with the same param pre-selected (ADR 0024). `enter_instrument`'s screen switch is gated on
        // a live engine link; the index targeting it sets is what we exercise here, link-free.
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        app.params = dither_candidates(&doc);
        app.loop_doc = Some(doc);
        app.tab = Tab::Params;

        let path = app.params.iter().find(|p| p.starts_with("instruments.")).cloned().expect("fixture has instrument params");
        let idx = app.params_display_order().iter().position(|p| *p == path).unwrap();
        app.params_state.select(Some(idx));

        app.open_selected_param_in_instrument();

        let inst = path.strip_prefix("instruments.").unwrap().split('.').next().unwrap();
        assert_eq!(app.selected_instrument().as_deref(), Some(inst), "the param's instrument is selected");
        assert_eq!(app.selected_param().as_deref(), Some(path.as_str()), "the same param row is pre-selected");
        assert_eq!(app.inst_params_state.selected(), Some(app.inst_param_idx), "scroll state tracks the pre-selected row");
    }

    #[test]
    fn scope_value_falls_back_to_base_for_static_params() {
        // Bug 1: the oscilloscope must plot any param, not just modulation destinations. A param
        // absent from the per-hop effective snapshot resolves to its authored base (a flat line).
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        let path = dither_candidates(&doc)
            .into_iter()
            .find(|p| p.starts_with("instruments."))
            .expect("fixture has instrument params");
        app.loop_doc = Some(doc);

        let empty = std::collections::BTreeMap::new();
        let base = app.scope_value(&empty, &path);
        assert!(base.is_some(), "unmodulated param resolves to its base, not None (no empty scope)");

        let mut eff = std::collections::BTreeMap::new();
        eff.insert(path.clone(), 0.42);
        assert_eq!(app.scope_value(&eff, &path), Some(0.42), "a modulated param uses its live effective value");
    }

    #[test]
    fn inst_param_list_scroll_state_follows_selection() {
        // Bug 4: the instrument param list is rendered stateful so the viewport scrolls with the
        // cursor (like the Params tab). The TableState selection must track `inst_param_idx`.
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        app.params = dither_candidates(&doc);
        app.loop_doc = Some(doc);
        // Target an instrument with at least two tunable params so a move is observable.
        let insts = app.instruments();
        let idx = insts
            .iter()
            .position(|name| app.params.iter().filter(|p| p.starts_with(&format!("instruments.{name}.params."))).count() >= 2)
            .expect("some instrument has >=2 tunable params");
        app.inst_idx = idx;

        app.move_inst_param(1);
        assert_eq!(app.inst_param_idx, 1);
        assert_eq!(app.inst_params_state.selected(), Some(1), "scroll state advances with the selection");
        app.move_inst_param(-1);
        assert_eq!(app.inst_params_state.selected(), Some(0), "scroll state follows back up");
    }

    #[test]
    fn constraint_commit_keeps_native_units() {
        // The constraint editor takes native units (Hz for cutoff_hz, what the screen shows) and
        // stores them as typed: schema v0.9 bands are native, the controller normalizes them.
        let mut app = App::new(Args::default());
        app.sample_rate = 48_000.0;
        let doc = fixture_doc();
        let cut = dither_candidates(&doc).into_iter().find(|p| p.ends_with(".cutoff_hz")).expect("fixture has a cutoff param");
        app.loop_doc = Some(doc);
        app.begin_edit(EditKind::Constraint, format!("{cut} 800 2000")); // 800 Hz .. 2 kHz
        app.commit_edit();
        assert!(app.edit_error.is_none(), "an in-range Hz band is accepted: {:?}", app.edit_error);
        let band = app.control.constraints.get(&cut).expect("constraint stored");
        assert_eq!((band.min, band.max), (Some(800.0), Some(2000.0)), "stored in Hz");
    }

    #[test]
    fn param_base_value_reads_operating_point_and_prefers_loop_doc_in_live_edit() {
        // The Params-tab base is the telemetry's operating point (where drift has walked it), falling
        // back to the loaded loop doc before the first loop renders. In LiveEdit the (live-watched)
        // doc wins so a hand-edited base shows immediately, not a render-ahead later (ADR 0024).
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        let p = "global.master_gain_db";
        let authored = doc.get_param(p).expect("fixture has master_gain_db");
        let cut = dither_candidates(&doc).into_iter().find(|x| x.ends_with(".cutoff_hz")).expect("fixture has a cutoff param");
        app.loop_doc = Some(doc);
        // No telemetry yet → the doc value.
        assert_eq!(app.param_base_value(p), Some(authored), "base falls back to the loaded loop doc");
        assert!(app.param_base_value(&cut).is_some(), "instrument param base also resolves from the doc");

        // Drift has walked the operating point: telemetry wins.
        let walked = authored - 3.0;
        let op = serde_json::json!({ "global": { "master_gain_db": walked } });
        app.telemetry = Some(Telemetry { operating_point: op, ..Default::default() });
        app.control.optimize = OptimizeMode::Drift;
        assert_eq!(app.param_base_value(p), Some(walked), "drift reads the rendered operating point");
        // A param missing from the mirror still falls back to the doc.
        assert!(app.param_base_value(&cut).is_some());

        // LiveEdit: the hand-edited doc beats the (render-ahead) operating point.
        app.control.optimize = OptimizeMode::LiveEdit;
        assert_eq!(app.param_base_value(p), Some(authored), "live-edit shows the doc value immediately");
    }

    #[test]
    fn value_at_navigates_native_unit_doc_mirror() {
        let doc = serde_json::json!({
            "global": { "master_gain_db": -1.94 },
            "instruments": { "poly": { "params": { "cutoff_hz": 551.6, "shift_st": -12.0 } } }
        });
        assert_eq!(value_at(&doc, "global.master_gain_db"), Some(-1.94));
        assert_eq!(value_at(&doc, "instruments.poly.params.cutoff_hz"), Some(551.6));
        assert_eq!(value_at(&doc, "instruments.poly.params.shift_st"), Some(-12.0), "native semitones, not normalized");
        assert_eq!(value_at(&doc, "instruments.poly.params.missing"), None);
    }

    fn record(loop_id: u64, path: &str, delta: f32) -> LoopRecord {
        LoopRecord {
            loop_id,
            revision: 0,
            optimize: "drift".into(),
            start_frame: 0,
            num_frames: 48_000,
            path: path.into(),
            delta,
            harmonic_move: None,
            categorical_move: None,
            song: None,
            peak: 0.5,
            doc_rev: 0,
            orrery: None,
            note_log: None,
        }
    }

    #[test]
    fn a_drift_step_prints_in_its_unit() {
        assert_eq!(fmt_step("instruments.p.params.cutoff_hz", 12.5), "+12.5Hz");
        assert_eq!(fmt_step("global.master_gain_db", -0.4), "-0.40dB");
        assert_eq!(fmt_step("instruments.p.params.attack_s", 0.05), "+0.050s");
        assert_eq!(fmt_step("instruments.p.params.shift_st", -1.0), "-1.00st");
        assert_eq!(fmt_step("instruments.p.params.resonance", 0.031), "+0.031", "a plain 0..1 param");
    }

    #[test]
    fn log_entry_renders_one_line_per_loop_record() {
        let mut r = record(2803, "instruments.poly.params.cutoff_hz", 12.5);
        r.start_frame = 48_000 * 90; // 1:30 into the session
        r.harmonic_move = Some(engines_core::orrery::HarmonicMove { chord: "Bm7".into(), tension: 0.4, default_chord: "I".into() });
        r.categorical_move = Some(engines_core::categorical::CategoricalMove {
            path: "instruments.poly.params.wave".into(),
            from: "saw_soft".into(),
            to: "sine".into(),
        });
        r.song = Some(engines_core::telemetry::SongStamp { block: 1, seam: true, ..Default::default() });
        let e = log_entry(&r, 48_000.0);
        assert_eq!(e.loop_id, 2803);
        assert!((e.at_s - 90.0).abs() < 1e-9, "position from start_frame / sample rate");
        assert_eq!(e.text, "drift · instruments.poly.params.cutoff_hz +12.5Hz · ♪Bm7 · ⇄ params.wave saw_soft→sine · ♫b2 seam · -6.0 dBFS");

        // A no-move hold loop is just the mode + peak; a silent loop reads -inf.
        let mut quiet = record(7, "", 0.0);
        quiet.optimize = "hold".into();
        quiet.peak = 0.0;
        assert_eq!(log_entry(&quiet, 48_000.0).text, "hold · -inf dBFS");
    }

    #[test]
    fn ingest_records_fills_bounded_log_and_counts_loops() {
        let mut app = App::new(Args::default());
        app.sample_rate = 48_000.0;
        let records: Vec<LoopRecord> = (0..(LOG_CAP as u64 + 10)).map(|i| record(i, "global.master_gain_db", 0.01)).collect();
        app.ingest_records(records);
        assert_eq!(app.loops, LOG_CAP as u64 + 10, "every rendered loop counts");
        assert_eq!(app.log.len(), LOG_CAP, "the log ring stays bounded");
        assert_eq!(app.log.back().map(|e| e.loop_id), Some(LOG_CAP as u64 + 9), "newest record kept last");
        assert_eq!(app.last_record.as_ref().map(|r| r.loop_id), Some(LOG_CAP as u64 + 9));
    }

    #[test]
    fn perturbed_param_comes_from_telemetry_and_sorts_first() {
        // "Drifted this loop" = `Telemetry::chosen_path`; its δ comes from the matching LoopRecord.
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        app.params = dither_candidates(&doc);
        app.loop_doc = Some(doc);
        let moved = app.params.iter().max().cloned().unwrap(); // alphabetically last ⇒ sorts last unmoved
        assert_ne!(app.params_display_order()[0], moved);

        app.telemetry = Some(Telemetry { loop_id: 5, chosen_path: moved.clone(), ..Default::default() });
        app.last_record = Some(record(5, &moved, -0.02));
        assert_eq!(app.perturbed_now(), Some(moved.as_str()));
        assert_eq!(app.params_display_order()[0], moved, "the drifted param sorts first");
        assert_eq!(app.perturbed_delta(&moved), Some(-0.02));
        assert_eq!(app.perturbed_delta("global.master_gain_db"), None, "only the moved param has a δ");

        // A record from a different loop than the telemetry snapshot never lends its δ.
        app.last_record = Some(record(4, &moved, -0.02));
        assert_eq!(app.perturbed_delta(&moved), None);

        // No move this loop ⇒ nothing is marked.
        app.telemetry = Some(Telemetry { loop_id: 6, ..Default::default() });
        assert_eq!(app.perturbed_now(), None);
    }

    #[test]
    fn viz_reveal_is_play_aligned_and_scope_matches_label() {
        // The oscilloscope (`scope`) and the param table's effective value (`latest_eff`) must come
        // from the SAME revealed hop, and a hop reveals only once its audio has played
        // (`audio_pos <= played`) — so the chart's newest sample equals the live label, in sync with
        // what's audible. Regression guard for "scope rising while the value label falls".
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        app.params = dither_candidates(&doc);
        app.loop_doc = Some(doc);
        let p = app.params.iter().find(|p| p.ends_with(".cutoff_hz")).cloned().expect("fixture has a cutoff param");
        let inst = p.strip_prefix("instruments.").unwrap().split('.').next().unwrap();
        app.inst_idx = app.instruments().iter().position(|n| n == inst).unwrap();
        app.inst_param_idx = app.inst_params().iter().position(|x| *x == p).unwrap();
        assert_eq!(app.selected_param().as_deref(), Some(p.as_str()));

        // Three hops with a rising effective value at increasing absolute audio positions.
        for (pos, val) in [(0u64, 0.2f32), (1920, 0.5), (3840, 0.8)] {
            let mut eff = std::collections::BTreeMap::new();
            eff.insert(p.clone(), val);
            app.viz_pending.push_back(VizFrame {
                loop_id: 0,
                hop_index: 0,
                num_frames: 1920,
                audio_pos: pos,
                master: Vec::new(),
                inst_dry: Vec::new(),
                eff,
            });
        }

        // Play cursor at 1920: only hops whose audio has been heard reveal (pos 0 and 1920).
        let n = app.reveal_viz(1920, 256);
        assert_eq!(n, 2, "only hops whose audio has played are revealed");
        assert_eq!(app.viz_pending.len(), 1, "the unheard hop (audio_pos 3840 > 1920) stays buffered");
        assert_eq!(app.scope.back().copied(), Some(0.5), "scope newest = last-played hop");
        assert_eq!(app.latest_eff().get(&p).copied(), Some(0.5), "param label = last-played hop");
        assert_eq!(
            app.scope.back().copied(),
            app.latest_eff().get(&p).copied(),
            "oscilloscope newest sample == param table value (no drift between them)",
        );

        // Advance the play cursor past the last hop; it reveals and both move together.
        assert_eq!(app.reveal_viz(5000, 256), 1);
        assert!(app.viz_pending.is_empty(), "all heard hops revealed");
        assert_eq!(app.scope.back().copied(), Some(0.8));
        assert_eq!(app.latest_eff().get(&p).copied(), Some(0.8));
    }

    #[test]
    fn enter_on_non_instrument_param_does_not_change_instrument_selection() {
        // A global/matrix/bus row has no per-instrument screen, so Enter is a no-op.
        let mut app = App::new(Args::default());
        let doc = fixture_doc();
        app.params = dither_candidates(&doc);
        app.loop_doc = Some(doc);
        app.tab = Tab::Params;
        app.inst_idx = 0;

        let path = "global.master_gain_db".to_string();
        let idx = app.params_display_order().iter().position(|p| *p == path).expect("master_gain_db is a candidate");
        app.params_state.select(Some(idx));

        app.open_selected_param_in_instrument();
        assert_eq!(app.inst_idx, 0, "a global param leaves the instrument selection unchanged");
    }

    // ── Phase 6d — structured constraint / override editors ──────────────────

    #[test]
    fn constraint_editor_sets_then_clears_band_and_bumps_revision() {
        let mut app = App::new(Args::default());
        app.control = SessionControl::default();
        app.begin_edit(EditKind::Constraint, "global.master_gain_db -6 -3".into());
        app.commit_edit();
        assert!(app.editing.is_none(), "a valid commit closes the editor");
        assert_eq!(app.control.constraints["global.master_gain_db"].min, Some(-6.0));
        assert_eq!(app.control.constraints["global.master_gain_db"].max, Some(-3.0));
        let rev = app.control.revision;
        app.begin_edit(EditKind::Constraint, "global.master_gain_db -".into());
        app.commit_edit();
        assert!(!app.control.constraints.contains_key("global.master_gain_db"), "`-` clears the band");
        assert!(app.control.revision > rev, "each commit bumps the revision");
    }

    #[test]
    fn override_editor_injects_value_and_bad_input_keeps_open() {
        let mut app = App::new(Args::default());
        app.control = SessionControl::default();
        app.begin_edit(EditKind::Override, "instruments.poly.params.cutoff_hz 1200".into());
        app.commit_edit();
        assert!(app.editing.is_none());
        assert_eq!(app.control.overrides["instruments.poly.params.cutoff_hz"].as_f64(), Some(1200.0));
        // a malformed line keeps the editor open with an error message
        app.begin_edit(EditKind::Override, "novalue".into());
        app.commit_edit();
        assert!(app.editing.is_some(), "a parse error keeps the editor open");
        assert!(app.edit_error.is_some());
    }

    #[test]
    fn short_path_keeps_last_two_segments() {
        assert_eq!(short_path("instruments.poly.params.cutoff_hz"), "params.cutoff_hz");
        assert_eq!(short_path("global.master_gain_db"), "global.master_gain_db");
        assert_eq!(short_path("solo"), "solo");
    }
}
