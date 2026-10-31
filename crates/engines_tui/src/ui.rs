//! Rendering. Immediate-mode: the whole screen is rebuilt each frame from `App` (its telemetry
//! snapshot, loop-record log, and control plane).
//! Sorting/derivation happens here (cheap: a few dozen rows) so state stays a plain model.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Sparkline, Table, Tabs};
use ratatui::Frame;

use crate::app::{fmt_step, App, Screen, SetupField, Tab, OPTIMIZE};
use engines_core::params::units;
use crate::viz::Spectro;
use engines_core::contract::OptimizeMode;
use engines_core::loop_doc::{param_norm, param_range};

pub fn draw(f: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Setup => draw_setup(f, app),
        Screen::Live => draw_live(f, app),
        Screen::Instrument => draw_instrument(f, app),
    }
}

// ── Setup ───────────────────────────────────────────────────────────────────

fn draw_setup(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(f.area());
    let s = &app.setup;

    // Show the parent dir + filename (e.g. `fixtures/loop_x.json`, `sandbox/loop_focus.json`) so the
    // source folder is clear once sandbox docs are in the list.
    let doc_name = s
        .docs
        .get(s.doc_idx)
        .map(|p| {
            let file = p.file_name().and_then(|x| x.to_str()).unwrap_or("?");
            match p.parent().and_then(|d| d.file_name()).and_then(|x| x.to_str()) {
                Some(dir) => format!("{dir}/{file}"),
                None => file.to_string(),
            }
        })
        .unwrap_or_else(|| "(none — pass --doc <path>)".to_string());

    // One row per visible field, in the registry's order (crate::setup::SETUP_FIELDS — the
    // same list `next_field` cycles, so focus and rows can't drift). Per-field semantics:
    // Return (ADR 0026 phase D) monitor-mixes the DAW's loopback return (needs an
    // `external_return` bus); MIDIdith lets drift move midi params; Output (ADR 0027)
    // "(default)" plays locally, anything else REDIRECTS the engine there (must differ
    // from Return).
    let mut lines = vec![Line::from("")];
    for fs in crate::setup::visible_fields(s.midi) {
        let focused = s.field == fs.id;
        lines.push(match fs.id {
            SetupField::Doc => selector(fs.label, &doc_name, s.doc_idx, s.docs.len(), focused),
            SetupField::Optimize => selector(fs.label, OPTIMIZE[s.optimize_idx], s.optimize_idx, OPTIMIZE.len(), focused),
            SetupField::Orrery => selector(fs.label, if s.orrery { "on" } else { "off" }, s.orrery as usize, 2, focused),
            SetupField::ReturnDevice => {
                let val = s.return_devices.get(s.return_idx).map(|x| x.as_str()).unwrap_or("(off)");
                selector(fs.label, val, s.return_idx, s.return_devices.len(), focused)
            }
            SetupField::MidiDither => selector(
                fs.label,
                if s.dither_midi { "on (midi drift)" } else { "off" },
                s.dither_midi as usize,
                2,
                focused,
            ),
            SetupField::Output => {
                let val = s.output_devices.get(s.output_idx).map(|x| x.as_str()).unwrap_or("(default)");
                selector(fs.label, val, s.output_idx, s.output_devices.len(), focused)
            }
        });
    }
    lines.push(Line::from(""));
    if let Some(err) = &s.error {
        lines.push(Line::from(Span::styled(format!("error: {err}"), Style::new().fg(Color::Red))));
    }

    let body = Paragraph::new(lines).block(Block::bordered().title(" engines-tui — live engine explorer "));
    f.render_widget(body, chunks[0]);
    f.render_widget(footer("Tab/⇧Tab: field · ←/→: change · Enter: start · Esc: quit"), chunks[1]);
}

fn selector<'a>(label: &'a str, value: &'a str, idx: usize, len: usize, focused: bool) -> Line<'a> {
    let count = if len > 0 { format!("  ({}/{})", idx + 1, len) } else { String::new() };
    Line::from(vec![
        Span::raw("  "),
        Span::styled(label, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(format!("◄ {value}{count} ►"), focus_style(focused)),
    ])
}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::new().fg(Color::Gray)
    }
}

// ── Live ──────────────────────────────────────────────────────────────────--

fn draw_live(f: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // tabs
        Constraint::Length(1), // status
        Constraint::Min(0),    // body
        Constraint::Length(1), // footer
    ])
    .split(f.area());

    let tab_idx = match app.tab {
        Tab::Params => 0,
        Tab::Log => 1,
    };
    let tabs = Tabs::new(vec![" Params (1) ", " Log (2) "])
        .select(tab_idx)
        .highlight_style(Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::bordered().title(" engines-tui "));
    f.render_widget(tabs, chunks[0]);

    // The param the controller stepped in the latest rendered loop (+ its native step, if numeric).
    let moving = app
        .perturbed_now()
        .map(|p| match app.perturbed_delta(p) {
            Some(d) => format!(" · moving {p} ({d:+.3})"),
            None => format!(" · moving {p}"),
        })
        .unwrap_or_default();
    // Live telemetry (Phase 1): Orrery chord/tension + any constraint corrections in flight.
    let tele = app
        .telemetry
        .as_ref()
        .map(|t| {
            let orr = t
                .orrery
                .as_ref()
                .map(|o| format!(" · ♪{} t{:.2}", o.chord, o.tension))
                .unwrap_or_default();
            let viol: Vec<&str> = t.constraints.iter().filter(|c| !c.in_range).map(|c| c.path.rsplit('.').next().unwrap_or("?")).collect();
            let vstr = if viol.is_empty() { String::new() } else { format!(" · ⤓correcting {}", viol.join(",")) };
            let rev = if t.revision > 0 { format!(" · rev{}", t.revision) } else { String::new() };
            format!("{rev}{orr}{vstr}")
        })
        .unwrap_or_default();
    // LiveEdit (ADR 0024): show the watched loop doc + any parse error from the last save.
    let live = if app.control.optimize == OptimizeMode::LiveEdit {
        let name = app.loop_doc_path.as_ref().and_then(|p| p.file_name()).and_then(|s| s.to_str()).unwrap_or("?");
        match &app.live_edit_error {
            Some(e) => format!(" · ◉watching {name} · ✗ {e}"),
            None => format!(" · ◉watching {name}"),
        }
    } else {
        String::new()
    };
    // Song form (ADR 0029): block/loop being rendered + play position against the song length.
    let song = app.song_status().map(|s| format!(" · {s}")).unwrap_or_default();
    let status = format!(
        "doc={} · mode={} · {} Hz · loops={}{}{}{}{}{}",
        app.doc_label,
        app.mode_label,
        app.sample_rate as u32,
        app.loops,
        song,
        moving,
        tele,
        live,
        if app.finished { " · FINISHED" } else { "" },
    );
    f.render_widget(Paragraph::new(status).style(Style::new().fg(Color::Gray)), chunks[1]);

    // A debug build renders slower than real time → the audio underruns. Make that obvious.
    if cfg!(debug_assertions) {
        let warn = Paragraph::new(" ⚠ debug build — audio will underrun; rebuild with --release (or `make tui`) ")
            .style(Style::new().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD));
        f.render_widget(warn, chunks[1]);
    }

    let ctl_keys = "o:optimize l:live-edit m:orrery c:constr v:override 0/d:open";
    match app.tab {
        Tab::Params => {
            draw_params(f, app, chunks[2]);
            f.render_widget(footer(&format!("↑/↓: select · Enter: inspect instrument · {ctl_keys} · Tab: next · q: quit")), chunks[3]);
        }
        Tab::Log => {
            draw_log(f, app, chunks[2]);
            f.render_widget(footer(&format!("{ctl_keys} · Tab: next · q: quit")), chunks[3]);
        }
    }

    // Inline structured editor (Phase 6d constraint/override): overrides the footer.
    if let Some(field) = &app.editing {
        let (label, hint) = field.kind.prompt();
        let err = app.edit_error.as_deref().map(|e| format!("  ✗ {e}")).unwrap_or_default();
        let buf = &field.buf;
        let line = Paragraph::new(format!(" {label} ▸ [{buf}_]  ({hint} · Enter: apply · Esc: cancel){err}"))
            .style(Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD));
        f.render_widget(line, chunks[3]);
    }
}

// ── Log tab: control panel + loop log (Phase 3) ──────────────────────────────--

fn draw_log(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(10), Constraint::Min(0)]).split(area);
    draw_control_panel(f, app, rows[0]);
    draw_loop_log(f, app, rows[1]);
}

/// Optimize / Orrery / constraint status from telemetry (ground truth), plus live overrides.
fn draw_control_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    // The watched objectives doc (Phase 1) — hand-edit it to re-steer; key `0` opens it in the OS editor.
    lines.push(match &app.objectives {
        Some(path) => Line::from(vec![
            Span::styled("objectives ", Style::new().fg(Color::Cyan)),
            Span::raw(path.clone()),
            Span::styled(
                if app.objectives_auto { "   [0: open · auto-created]" } else { "   [0: open in editor]" },
                Style::new().fg(Color::DarkGray),
            ),
        ]),
        None => Line::from(vec![
            Span::styled("objectives ", Style::new().fg(Color::Cyan)),
            Span::styled("(auto-created in the cwd on start; key 0 opens it)", Style::new().fg(Color::DarkGray)),
        ]),
    });
    if let Some(t) = &app.telemetry {
        lines.push(Line::from(vec![
            Span::styled("optimize ", Style::new().fg(Color::Cyan)),
            Span::raw(format!("{}   ", t.optimize)),
            Span::styled("rev ", Style::new().fg(Color::Cyan)),
            Span::raw(format!("{}   ", t.revision)),
            Span::styled("loop ", Style::new().fg(Color::Cyan)),
            Span::raw(t.loop_id.to_string()),
        ]));
        if let Some(o) = &t.orrery {
            let leader = o.leader.as_deref().map(|l| format!("  ◀leader {l}")).unwrap_or_default();
            let drop = o.dropped.as_deref().map(|d| format!("  ⊘drop {d}")).unwrap_or_default();
            let home = if o.returning_home { "  ⌂returning-home" } else { "" };
            let phrase = o.phrase_active.as_deref().map(|p| format!("  ♫{p}")).unwrap_or_default();
            lines.push(Line::from(format!(
                "orrery   ♪ {}  tension {:.2}  lsh {}  Δvoices {:.1}  energy {:.2}  arp×{}{}{}{}{}",
                o.chord, o.tension, o.loops_since_home, o.voice_change_avg, o.energy, o.arp_pulses, phrase, leader, drop, home
            )));
        }
        if t.constraints.is_empty() {
            lines.push(Line::from(Span::styled("constraints  (none)", Style::new().fg(Color::DarkGray))));
        }
        for c in &t.constraints {
            let band = format!("[{}, {}]", c.min.map(|m| format!("{m:.2}")).unwrap_or_else(|| "·".into()), c.max.map(|m| format!("{m:.2}")).unwrap_or_else(|| "·".into()));
            let (tag, col) = if c.in_range { ("ok", Color::Green) } else { ("⤓correcting", Color::Yellow) };
            lines.push(Line::from(vec![
                Span::raw(format!("constr   {} = {:.3} {band}  ", c.path, c.value)),
                Span::styled(tag, Style::new().fg(col)),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled("(telemetry pending — first loop not yet rendered)", Style::new().fg(Color::DarkGray))));
    }
    // Live overrides (Phase 6d): the one-shot value overlays the user has set (`v` to add, `path -` clears).
    if app.control.overrides.is_empty() {
        lines.push(Line::from(Span::styled("overrides    (none)", Style::new().fg(Color::DarkGray))));
    } else {
        for (path, val) in &app.control.overrides {
            lines.push(Line::from(Span::raw(format!("override {path} = {val}"))));
        }
    }
    let p = Paragraph::new(lines).block(Block::bordered().title(" Control / constraints / overrides (live) "));
    f.render_widget(p, area);
}

/// One row per rendered loop (newest first): loop id, audio position, and what happened.
fn draw_loop_log(f: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .log
        .iter()
        .rev() // newest first
        .map(|e| {
            Row::new(vec![
                Cell::from(e.loop_id.to_string()),
                Cell::from(engines_core::song::fmt_hms(e.at_s)),
                Cell::from(e.text.clone()),
            ])
        })
        .collect();
    let table = Table::new(rows, [Constraint::Length(8), Constraint::Length(9), Constraint::Min(10)])
        .header(header(["loop", "at", "mode · move · chord · song · peak"]))
        .block(Block::bordered().title(" Loop log (newest first; rendered ahead of playback) "));
    f.render_widget(table, area);
}

fn draw_params(f: &mut Frame, app: &mut App, area: Rect) {
    // Row order is computed on `App` so the Enter handler and this table agree on the selection.
    let order = app.params_display_order();
    // Base values need whole-`app` access (operating point + loop doc), so resolve them before the
    // field destructure below. The loop-doc fallback makes a hand-edited base show in LiveEdit.
    let base_vals: Vec<Option<f32>> = order.iter().map(|p| app.param_base_value(p)).collect();
    // The drifted-this-loop param and its native step, resolved before the field destructure below.
    let perturbed = app.perturbed_now().map(str::to_string);
    let pert_delta = perturbed.as_deref().and_then(|p| app.perturbed_delta(p));
    let App { params_state, telemetry, .. } = app;

    // Per-instrument audibility (S1-h5): the param table shows knob positions, not whether a voice is
    // sounding. Join each instrument param's owning track so a voice silent in the audio (no notes, an
    // Orrery drop, euclid pulses=0, a returned-home rest) is flagged even though its `level_db` reads full.
    let track_map: std::collections::HashMap<&str, (bool, f32, bool)> = telemetry
        .as_ref()
        .map(|t| t.tracks.iter().map(|tr| (tr.name.as_str(), (tr.enabled, tr.rms, tr.audible))).collect())
        .unwrap_or_default();
    let inst_of = |path: &str| -> Option<String> {
        path.strip_prefix("instruments.").and_then(|r| r.split('.').next()).map(str::to_string)
    };

    let rows: Vec<Row> = order
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let is_pert = perturbed.as_deref() == Some(p.as_str());
            let delta = if is_pert { pert_delta.map(|d| fmt_step(p, d)).unwrap_or_default() } else { String::new() };
            let marker = if is_pert { "●" } else { "" };
            // Audibility of the owning instrument (blank for global/matrix/bus params).
            let track = inst_of(p).and_then(|name| track_map.get(name.as_str()).copied());
            let (aud_text, aud_style) = match track {
                None => (String::new(), Style::new()),
                Some((false, _, _)) => ("off".to_string(), Style::new().fg(Color::DarkGray)),
                Some((true, _, false)) => ("MUTE".to_string(), Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Some((true, rms, true)) => (format!("{:.2}", rms.min(9.99)), Style::new().fg(Color::DarkGray)),
            };
            let inaudible = matches!(track, Some((false, _, _)) | Some((true, _, false)));
            let style = if is_pert {
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else if inaudible {
                Style::new().fg(Color::DarkGray) // knob set, but the voice is silent in the audio
            } else {
                Style::new()
            };
            Row::new(vec![
                Cell::from(p.as_str().to_string()),
                Cell::from(fmt_base(p, base_vals[i])),
                Cell::from(delta),
                Cell::from(aud_text).style(aud_style),
                Cell::from(marker),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Min(28), Constraint::Length(9), Constraint::Length(9), Constraint::Length(5), Constraint::Length(3)],
    )
    .header(header(["param", "base", "δ", "aud", "·"]))
    .block(Block::bordered().title(" Tuneable params — drifted-this-loop first (native units; δ = this loop's step; aud = dry RMS / MUTE / off) "))
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ");
    f.render_stateful_widget(table, area, params_state);
}

// ── small helpers ─────────────────────────────────────────────────────────--

fn header<'a, const N: usize>(cells: [&'a str; N]) -> Row<'a> {
    Row::new(cells).style(Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan))
}

fn footer(text: &str) -> Paragraph<'_> {
    Paragraph::new(text).style(Style::new().fg(Color::DarkGray))
}

/// A param's base as the doc writes it, in its unit (`800Hz`, `-6.0dB`, `-∞dB`, `0.300`).
fn fmt_base(path: &str, v: Option<f32>) -> String {
    v.map(|x| units::fmt_authored(path, x)).unwrap_or_else(|| "—".to_string())
}

// ── Instrument live-stats screen (ADR 0024) ─────────────────────────────────

pub fn draw_instrument(f: &mut Frame, app: &mut App) {
    let rows = Layout::vertical([
        Constraint::Length(1), // header (instrument selector)
        Constraint::Min(6),    // dry spectrogram (selected instrument)
        Constraint::Min(6),    // master spectrogram
        Constraint::Length(9), // params | oscilloscope
        Constraint::Length(1), // selected-param tier detail (native/safe/constraint)
        Constraint::Length(1), // footer / modal editor
    ])
    .split(f.area());

    // Header: ◀ instrument ▶ (i/N), with a MIDI badge for translator instruments (ADR 0026 —
    // they are silent by design, so the dry spectrogram showing nothing is expected, not a bug).
    let insts = app.instruments();
    let sel = app.selected_instrument().unwrap_or_else(|| "—".into());
    let is_midi = app
        .loop_doc
        .as_ref()
        .and_then(|d| d.instruments.get(&sel))
        .map(|i| i.is_midi())
        .unwrap_or(false);
    let mut spans = vec![
        Span::styled("instrument ", Style::new().fg(Color::DarkGray)),
        Span::raw("◀ "),
        Span::styled(sel.clone(), Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" ▶ "),
        Span::styled(format!("({}/{})", app.inst_idx + 1, insts.len().max(1)), Style::new().fg(Color::DarkGray)),
    ];
    if is_midi {
        spans.push(Span::styled("  MIDI→ext (no audio)", Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    let dry_title = if is_midi {
        format!("{sel} — dry (MIDI translator: silent by design)")
    } else {
        format!("{sel} — dry (when this voice plays)")
    };
    draw_spectrogram(f, rows[1], app.dry_spectro(), &dry_title);
    draw_spectrogram(f, rows[2], app.master_spectro(), "master mix");

    let bottom = Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).split(rows[3]);
    draw_inst_params(f, bottom[0], app);
    draw_oscilloscope(f, bottom[1], app);

    // Tier detail for the selected param (Tier-1 native / Tier-2 safe / Tier-3 constraint, ADR 0024).
    f.render_widget(Paragraph::new(selected_param_detail(app)), rows[4]);

    // Footer, or the modal constraint editor when open (same line/style as the Live editor).
    if let Some(field) = &app.editing {
        let (label, hint) = field.kind.prompt();
        let err = app.edit_error.as_deref().map(|e| format!("  ✗ {e}")).unwrap_or_default();
        let buf = &field.buf;
        let line = Paragraph::new(format!(" {label} ▸ [{buf}_]  ({hint} · Enter: apply · Esc: cancel){err}"))
            .style(Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD));
        f.render_widget(line, rows[5]);
    } else {
        f.render_widget(footer("↑/↓: param · ←/→: instrument · c: edit-constraint · i/Esc: back · q: quit"), rows[5]);
    }
}

/// One-line tier detail for the selected param: Tier-1 native range, Tier-2 safe range, Tier-3
/// constraint band — all shown in **real units** (Hz/s/ms/st/×), not raw 0..1 storage values
/// (ADR 0024). `param_range`/`safe_range`/constraints are in the param's canonical space (0..1 for
/// internally-normalized params, native only for the seam params), so each bound is re-normalized
/// then formatted through the units registry (`fmt_native`).
fn selected_param_detail(app: &App) -> Line<'static> {
    let nyq = (app.sample_rate as f32 * 0.5).max(1.0);
    let Some(p) = app.selected_param() else { return Line::from("") };
    let nat = |v: f32| units::fmt_native(&p, param_norm(&p, v), nyq);
    let (t1lo, t1hi) = param_range(&p);
    let (t2lo, t2hi) = app.loop_doc.as_ref().map(|d| d.safe_range(&p)).unwrap_or((t1lo, t1hi));
    let cstr = match app.control.constraints.get(&p) {
        Some(b) => format!(
            "[{}..{}]",
            b.min.map(nat).unwrap_or_else(|| "-".into()),
            b.max.map(nat).unwrap_or_else(|| "-".into()),
        ),
        None => "(none)".into(),
    };
    Line::from(vec![
        Span::styled(format!("{} ", units::leaf(&p)), Style::new().fg(Color::Cyan)),
        Span::styled(format!("native[{}..{}] ", nat(t1lo), nat(t1hi)), Style::new().fg(Color::DarkGray)),
        Span::styled(format!("safe[{}..{}] ", nat(t2lo), nat(t2hi)), Style::new().fg(Color::Yellow)),
        Span::styled(format!("constraint{cstr} "), Style::new().fg(Color::Green)),
        Span::styled(format!("· {}", units::unit_label(&p)), Style::new().fg(Color::DarkGray)),
    ])
}

/// Render a rolling spectrogram into `area`: newest column at the right, low freq at the bottom.
fn draw_spectrogram(f: &mut Frame, area: Rect, spectro: Option<&Spectro>, title: &str) {
    let block = Block::bordered().title(title).border_style(Style::new().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (Some(sp), w, h) = (spectro, inner.width as usize, inner.height as usize) else { return };
    if w == 0 || h == 0 {
        return;
    }
    // Just opened (or just switched/cleared): the rolling spectrogram is empty for ~the render-ahead
    // while it aligns to the play cursor (ADR 0024). A hint keeps the blank panel from reading as broken.
    if sp.cols().is_empty() {
        let hint = Paragraph::new("⏳ aligning to playback…")
            .style(Style::new().fg(Color::DarkGray))
            .alignment(ratatui::layout::Alignment::Center);
        let mid = Rect { x: inner.x, y: inner.y + inner.height / 2, width: inner.width, height: 1 };
        f.render_widget(hint, mid);
        return;
    }
    let cols: Vec<&Vec<f32>> = sp.cols().iter().collect();
    let ncols = cols.len();
    let srows = sp.rows().max(1);
    let mut lines = Vec::with_capacity(h);
    for y in 0..h {
        // y=0 (top) = high freq = spectro row srows-1; y=h-1 (bottom) = low freq = row 0.
        let srow = if h <= 1 { srows - 1 } else { ((srows - 1) * (h - 1 - y)) / (h - 1) };
        let mut spans = Vec::with_capacity(w);
        for x in 0..w {
            let from_right = w - 1 - x; // 0 at the right edge (newest)
            let v = if from_right < ncols { cols[ncols - 1 - from_right][srow] } else { 0.0 };
            spans.push(Span::styled(" ", Style::default().bg(heat(v))));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Param table for the selected instrument: leaf name, effective (post-mod) native value, unit. A
/// `~` marks a param that is live-modulated this loop (has an oscilloscope), else it's static.
/// Rendered stateful (`inst_params_state`) so the viewport scrolls to follow the selection, like
/// the Params tab (ADR 0024).
fn draw_inst_params(f: &mut Frame, area: Rect, app: &mut App) {
    let nyq = (app.sample_rate as f32 * 0.5).max(1.0);
    let sel = app.inst_param_idx;
    // Scope the immutable borrows of `app` so the trailing `&mut app.inst_params_state` is clean.
    let rows: Vec<Row> = {
        let params = app.inst_params();
        let eff = app.latest_eff();
        params
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let leaf = units::leaf(path);
                // Live (modulated) value if present, else the static authored value (normalized).
                let (vnorm, live) = match eff.get(path) {
                    Some(&v) => (v, true),
                    None => (
                        app.loop_doc.as_ref().and_then(|d| d.get_param(path)).map(|n| param_norm(path, n)).unwrap_or(0.0),
                        false,
                    ),
                };
                let marker = if live { "~" } else { " " };
                let val = units::fmt_native(path, vnorm, nyq);
                let style = if i == sel {
                    Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else if live {
                    Style::new().fg(Color::White)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                Row::new(vec![
                    Cell::from(format!("{marker} {leaf}")),
                    Cell::from(val),
                    Cell::from(units::unit_label(path)),
                ])
                .style(style)
            })
            .collect()
    };
    let table = Table::new(rows, [Constraint::Min(12), Constraint::Length(9), Constraint::Length(5)])
        .header(Row::new(vec!["param", "eff", "unit"]).style(Style::new().fg(Color::DarkGray)))
        .block(Block::bordered().title("params").border_style(Style::new().fg(Color::DarkGray)));
    f.render_stateful_widget(table, area, &mut app.inst_params_state);
}

/// Oscilloscope of the selected param's effective value over time (normalized 0..1 → bar height).
fn draw_oscilloscope(f: &mut Frame, area: Rect, app: &App) {
    let nyq = (app.sample_rate as f32 * 0.5).max(1.0);
    let title = match app.selected_param() {
        Some(p) => {
            let now = app.scope.back().copied().unwrap_or(0.0);
            format!("{} = {} (25 Hz)", units::leaf(&p), units::fmt_native(&p, now, nyq))
        }
        None => "oscilloscope".into(),
    };
    let block = Block::bordered().title(title).border_style(Style::new().fg(Color::DarkGray));
    // Feed the Sparkline only the newest `width` samples: ratatui renders `data[0..width]` (the
    // *oldest* `width`), so passing the whole ring would show ~SCOPE_CAP-stale bars while the title +
    // param table show the current value — the chart appearing to rise while the live value falls. The
    // windowed tail puts the newest sample at the right edge, matching the label (play-cursor aligned).
    let bars = block.inner(area).width as usize;
    let data = scope_bars(&app.scope, bars);
    let spark = Sparkline::default().block(block).data(&data).max(100).style(Style::new().fg(Color::Green));
    f.render_widget(spark, area);
}

/// The newest `width` oscilloscope samples (normalized 0..1) as 0..100 bar heights, in chronological
/// order so the newest is last (rendered at the right edge by the default left-to-right Sparkline).
fn scope_bars(scope: &std::collections::VecDeque<f32>, width: usize) -> Vec<u64> {
    let mut bars: Vec<u64> = scope.iter().rev().take(width).map(|v| (v.clamp(0.0, 1.0) * 100.0) as u64).collect();
    bars.reverse();
    bars
}

/// Spectrogram magnitude (0..1) → a perceptual heat color (black→blue→cyan→green→yellow→red).
fn heat(v: f32) -> Color {
    let v = v.clamp(0.0, 1.0);
    const STOPS: [(f32, (u8, u8, u8)); 6] = [
        (0.00, (8, 8, 18)),
        (0.25, (30, 30, 120)),
        (0.45, (10, 110, 160)),
        (0.65, (20, 170, 110)),
        (0.82, (210, 200, 60)),
        (1.00, (235, 70, 35)),
    ];
    let (mut lo, mut hi) = (&STOPS[0], &STOPS[STOPS.len() - 1]);
    for pair in STOPS.windows(2) {
        if v >= pair[0].0 && v <= pair[1].0 {
            lo = &pair[0];
            hi = &pair[1];
            break;
        }
    }
    let t = if (hi.0 - lo.0).abs() < 1e-6 { 0.0 } else { (v - lo.0) / (hi.0 - lo.0) };
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color::Rgb(lerp(lo.1 .0, hi.1 .0), lerp(lo.1 .1, hi.1 .1), lerp(lo.1 .2, hi.1 .2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Args;
    use engines_core::controller::dither_candidates;

    use crate::test_support::fixture_doc;

    #[test]
    fn selected_param_detail_shows_native_units_not_normalized() {
        // Bug 5: a Hz param's native/safe ranges must read in real units, not 0..1 storage values.
        let mut app = App::new(Args::default());
        app.sample_rate = 48_000.0;
        let doc = fixture_doc();
        app.params = dither_candidates(&doc);
        app.loop_doc = Some(doc);
        // Select a cutoff (Hz) param and confirm the tier line renders in Hz.
        let cutoff = app.params.iter().find(|p| p.ends_with(".cutoff_hz")).cloned().expect("fixture has a cutoff param");
        let inst = cutoff.strip_prefix("instruments.").unwrap().split('.').next().unwrap();
        app.inst_idx = app.instruments().iter().position(|n| n == inst).unwrap();
        app.inst_param_idx = app.inst_params().iter().position(|p| *p == cutoff).unwrap();

        let line = selected_param_detail(&app);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Hz"), "native/safe ranges render in Hz, got: {text}");
        assert!(!text.contains("0.00..1.00"), "must not show raw normalized 0..1 bounds, got: {text}");
    }

    #[test]
    fn a_base_prints_as_the_doc_writes_it() {
        assert_eq!(fmt_base("instruments.p.params.cutoff_hz", Some(800.0)), "800Hz");
        assert_eq!(fmt_base("instruments.p.params.level_db", Some(f32::NEG_INFINITY)), "-∞dB");
        assert_eq!(fmt_base("instruments.p.params.resonance", Some(0.3)), "0.300");
        assert_eq!(fmt_base("global.master_gain_db", None), "—");
    }

    #[test]
    fn oscilloscope_windows_to_newest_samples_at_right_edge() {
        // ratatui's Sparkline renders the FIRST `width` samples (the oldest). The oscilloscope must
        // window to the newest `width` so the rightmost bar is the current value (matching the title +
        // param label), not ~SCOPE_CAP-stale data — the "rises while the live value falls" bug.
        use std::collections::VecDeque;
        let scope: VecDeque<f32> = (0..240).map(|i| i as f32 / 239.0).collect(); // strictly rising 0→1
        let bars = scope_bars(&scope, 40);
        assert_eq!(bars.len(), 40, "windowed to the chart width");
        assert_eq!(*bars.last().unwrap(), 100, "newest sample (1.0) is the rightmost bar");
        assert!(bars.first().unwrap() < bars.last().unwrap(), "recent tail: older→newer, left→right");
        // The window is the TAIL (starts `width` back from newest), not scope[0].
        let expected_first = (scope[240 - 40].clamp(0.0, 1.0) * 100.0) as u64;
        assert_eq!(*bars.first().unwrap(), expected_first, "window starts width-back from newest, not at the oldest sample");
    }

    #[test]
    fn scope_bars_shows_all_when_shorter_than_width() {
        use std::collections::VecDeque;
        let scope: VecDeque<f32> = [0.1, 0.5, 0.9].into_iter().collect();
        assert_eq!(scope_bars(&scope, 40), vec![10, 50, 90], "fewer samples than width: all shown, newest last");
    }
}
