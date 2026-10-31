//! `engines-song voyage <doc>`: the Orrery's chord voyage, tabulated as `orrery_voyage.py`
//! did from a `--loop-log`. The controller runs headless and renders nothing: the Orrery's picks
//! never depend on the audio, so stepping the controller (and a song's block driver) loop by loop
//! makes exactly the moves an `engines-song --orrery` render would log.
//!
//! A song-less doc runs `--loops` (20); a song plays scaled down like `seams` (every block, the
//! longest `--longest` loops), or `--full`. `--movement default` replaces the doc's movement with
//! the Orrery's defaults (a chord change every loop): the worst case a `context` floor must hold.

use std::collections::BTreeMap;

use engines_core::contract::SessionControl;
use engines_core::controller::{Controller, DitherConfig};
use engines_core::engine::PhaseSnapshot;
use engines_core::loop_doc::LoopDoc;
use engines_core::song::SongDriver;
use serde::Serialize;

use super::{control, default_movement, load, scale_song, Common, Findings, Scale, Scaled};
use crate::Fail;

#[derive(clap::Args, Debug, PartialEq)]
pub struct VoyageArgs {
    #[command(flatten)]
    pub common: Common,
    /// A song-less doc: loops to run
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..))]
    pub loops: u64,
    /// The Orrery's movement: the doc's own, or `default` (a chord change every loop — the worst
    /// case a `context` floor must hold)
    #[arg(long, value_enum, default_value_t = MovementArg::Doc)]
    pub movement: MovementArg,
    /// The timbre axis while it runs
    #[arg(long, default_value = "hold", value_parser = ["hold", "drift"])]
    pub optimize: String,
    #[command(flatten)]
    pub scale: Scale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MovementArg {
    Doc,
    Default,
}

/// One loop of the voyage.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub loop_id: u64,
    /// The song block (the last block's during the tail); `None` for a song-less doc.
    pub block: Option<usize>,
    /// A block's first loop (its seam glide).
    pub seam: bool,
    pub tail: bool,
    /// The Orrery's transfer this loop, when it moved.
    pub chord: Option<String>,
    pub tension: Option<f64>,
    pub default_chord: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub doc: String,
    pub movement: MovementArg,
    pub optimize: String,
    pub seed: u64,
    pub scaled: Option<Scaled>,
    pub rows: Vec<Row>,
    /// The whole voyage; `None` when the Orrery never moved.
    pub summary: Option<Summary>,
    /// A song: each block's own voyage (each restarts from home on its own seed).
    pub blocks: Vec<BlockVoyage>,
    /// Recaps and shared openings found from the song's explicit seeds.
    pub recaps: Vec<String>,
    #[serde(flatten)]
    pub findings: Findings,
    /// `TAMED`, `ROAMING` or `NO MOVES` (`… with warnings` after a recap that doesn't open alike).
    pub verdict: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    /// Each chord and its count, in order of first visit.
    pub distinct: Vec<(String, usize)>,
    pub tension_min: f64,
    pub tension_mean: f64,
    pub tension_max: f64,
    /// The longest run of consecutive moves above 0.2 tension.
    pub longest_excursion: usize,
    pub most_visited: (String, usize),
    pub moves: usize,
    /// Mean ≤ 0.12, max ≤ 0.45 and no excursion over 2 loops.
    pub tamed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BlockVoyage {
    pub block: usize,
    pub loops: usize,
    /// `None`: the Orrery didn't move in this block.
    pub summary: Option<Summary>,
    /// Under 8 moves: the numbers are only indicative.
    pub indicative: bool,
    /// The block's first three chords.
    pub opens: Vec<String>,
}

pub fn run(a: VoyageArgs) -> Result<bool, Fail> {
    let (doc, _) = load(&a.common.doc)?;
    let (doc, scaled) = match scale_song(&doc, &a.scale) {
        Some((d, s)) => (d, Some(s)),
        None => (doc, None),
    };
    let (max_change, mut ctl) = control(&doc, &a.optimize, true)?;
    if a.movement == MovementArg::Default {
        ctl = default_movement(ctl);
    }
    let rows = run_controller(doc.clone(), ctl, max_change, a.common.seed, a.loops as usize, a.common.sample_rate)?;
    let moves: Vec<(String, f64)> = rows.iter().filter_map(|r| Some((r.chord.clone()?, r.tension?))).collect();
    let summary = summarize(&moves);
    let mut findings = Findings::default();
    let (blocks, recaps) = if doc.song.is_some() { per_block(&rows, &doc, &mut findings) } else { (Vec::new(), Vec::new()) };
    let verdict = match &summary {
        None => "NO MOVES",
        Some(s) if s.tamed => "TAMED",
        Some(_) => "ROAMING",
    };
    let report = Report {
        doc: a.common.doc.clone(),
        movement: a.movement,
        optimize: a.optimize.clone(),
        seed: a.common.seed,
        scaled,
        rows,
        summary,
        blocks,
        recaps,
        verdict: findings.verdict(verdict),
        findings,
    };
    super::emit(a.common.json, &report, print)?;
    // TAMED / ROAMING is the voyage's verdict, not a failure: a piece may want to roam.
    Ok(true)
}

/// The Session's offline loop without the engine: the song hook at each loop top, the pick, the
/// re-base. `loops` caps a song-less doc; a song runs to the end of its tail.
fn run_controller(doc: LoopDoc, control: Option<SessionControl>, max_change: f32, seed: u64, loops: usize, sr: f64) -> Result<Vec<Row>, Fail> {
    let control = control.or_else(|| SessionControl::from_doc(&doc));
    let mut doc = doc;
    let mut song = doc.take_song().map(|spec| SongDriver::new(&doc, &spec, seed, sr)).transpose().map_err(Fail::Usage)?;
    let dither = DitherConfig { max_change_per_loop: max_change, seed, ..Default::default() };
    let (doc, dither, total) = match &song {
        Some(s) => (s.block_doc(0).clone(), DitherConfig { seed: s.block_seed(0), ..dither }, s.total_loops()),
        None => (doc, dither, loops),
    };
    let base = doc.loop_id;
    let mut ctl = Controller::new(doc, dither);
    ctl.set_control(control);
    let mut snap = PhaseSnapshot::empty();
    let mut rows = Vec::with_capacity(total);
    for n in 0..total {
        let pos = song.as_mut().map(|s| s.on_loop_top(n, &mut ctl, &mut snap));
        // The render would consume the seam it armed; nothing else reads the snapshot here.
        snap.seam = None;
        let loop_id = base + n as u64;
        let p = ctl.next(loop_id);
        let hm = p.harmonic_move.clone();
        rows.push(Row {
            loop_id,
            block: pos.as_ref().map(|t| t.block),
            seam: pos.as_ref().is_some_and(|t| t.seam),
            tail: pos.as_ref().is_some_and(|t| t.tail),
            // As a loop log carries it: the f32 written shortest, read back as f64.
            tension: hm.as_ref().map(|h| format!("{}", h.tension).parse().unwrap_or(h.tension as f64)),
            chord: hm.as_ref().map(|h| h.chord.clone()),
            default_chord: hm.map(|h| h.default_chord),
        });
        ctl.rebase(p.doc);
    }
    Ok(rows)
}

/// `orrery_voyage.py`'s `summary`: the distinct chords, tension stats, the longest excursion above
/// 0.2 and the tamed verdict.
pub fn summarize(moves: &[(String, f64)]) -> Option<Summary> {
    if moves.is_empty() {
        return None;
    }
    let mut distinct: Vec<(String, usize)> = Vec::new();
    for (c, _) in moves {
        match distinct.iter_mut().find(|(d, _)| d == c) {
            Some((_, n)) => *n += 1,
            None => distinct.push((c.clone(), 1)),
        }
    }
    let ts: Vec<f64> = moves.iter().map(|(_, t)| *t).collect();
    let (mut run, mut best) = (0usize, 0usize);
    for &t in &ts {
        run = if t > 0.2 { run + 1 } else { 0 };
        best = best.max(run);
    }
    let mean = ts.iter().sum::<f64>() / ts.len() as f64;
    let max = ts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // Counter.most_common(1): the highest count, the first seen on a tie.
    let most = distinct.iter().fold(None::<&(String, usize)>, |b, d| match b {
        Some(b) if b.1 >= d.1 => Some(b),
        _ => Some(d),
    });
    Some(Summary {
        tension_min: ts.iter().copied().fold(f64::INFINITY, f64::min),
        tension_mean: mean,
        tension_max: max,
        longest_excursion: best,
        most_visited: most.cloned().unwrap_or_default(),
        moves: moves.len(),
        tamed: mean <= 0.12 && max <= 0.45 && best <= 2,
        distinct,
    })
}

/// Each block's voyage (the tail counts with the last block), then the recaps the doc's explicit
/// seeds make and the openings that only coincide.
fn per_block(rows: &[Row], doc: &LoopDoc, findings: &mut Findings) -> (Vec<BlockVoyage>, Vec<String>) {
    let mut order: Vec<usize> = Vec::new();
    let mut by_block: BTreeMap<usize, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        let Some(b) = r.block else { continue };
        if !by_block.contains_key(&b) {
            order.push(b);
        }
        by_block.entry(b).or_default().push(r);
    }
    let mut opening: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let blocks: Vec<BlockVoyage> = order
        .iter()
        .map(|&b| {
            let rs = &by_block[&b];
            let mv: Vec<(String, f64)> = rs.iter().filter_map(|r| Some((r.chord.clone()?, r.tension?))).collect();
            let opens: Vec<String> = mv.iter().take(3).map(|(c, _)| c.clone()).collect();
            if !mv.is_empty() {
                opening.insert(b, opens.clone());
            }
            BlockVoyage { block: b, loops: rs.len(), indicative: mv.len() < 8, summary: summarize(&mv), opens }
        })
        .collect();

    let voyage = |b: usize| opening.get(&b).map_or("no moves".to_string(), |o| o.join(" → "));
    let names = |bs: &[usize]| bs.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
    let mut notes = Vec::new();
    let seq = doc.song.as_ref().map(|s| s.sequence.as_slice()).unwrap_or_default();
    let mut by_seed: Vec<(u64, Vec<usize>)> = Vec::new();
    for (i, blk) in seq.iter().enumerate() {
        if let (Some(seed), true) = (blk.seed, by_block.contains_key(&i)) {
            match by_seed.iter_mut().find(|(s, _)| *s == seed) {
                Some((_, bs)) => bs.push(i),
                None => by_seed.push((seed, vec![i])),
            }
        }
    }
    let mut recapped: Vec<usize> = Vec::new();
    for (seed, bs) in &by_seed {
        if bs.len() < 2 {
            continue;
        }
        recapped.extend(bs);
        let first = opening.get(&bs[0]);
        if bs.iter().all(|b| opening.get(b) == first) {
            notes.push(format!("recap: blocks {} share explicit seed {seed} and open with the same voyage ({})", names(bs), voyage(bs[0])));
        } else {
            let each = bs.iter().map(|&b| format!("{b}: {}", voyage(b))).collect::<Vec<_>>().join("; ");
            findings.warns.push(format!("blocks {} share explicit seed {seed} but open differently ({each}) — not heard as a recap", names(bs)));
        }
    }
    let mut same: Vec<(Vec<String>, Vec<usize>)> = Vec::new();
    for (&b, o) in &opening {
        if o.len() == 3 {
            match same.iter_mut().find(|(x, _)| x == o) {
                Some((_, bs)) => bs.push(b),
                None => same.push((o.clone(), vec![b])),
            }
        }
    }
    for (o, bs) in &same {
        if bs.len() > 1 && !bs.iter().all(|b| recapped.contains(b)) {
            notes.push(format!("blocks {} open alike ({}) without a shared explicit `seed` — a coincidence, not a recap", names(bs), o.join(" → ")));
        }
    }
    (blocks, notes)
}

fn print(r: &Report) {
    let what = match (&r.scaled, r.rows.first().and_then(|x| x.block)) {
        (Some(_), _) | (None, Some(_)) => "the song".to_string(),
        (None, None) => format!("{} loops", r.rows.len()),
    };
    println!(
        "{} · {what} · optimize {} · movement {} · seed {:#x}",
        r.doc,
        r.optimize,
        match r.movement {
            MovementArg::Doc => "the doc's",
            MovementArg::Default => "default (a chord change every loop)",
        },
        r.seed
    );
    if let Some(s) = &r.scaled {
        s.print(&r.doc);
    }
    let is_song = r.rows.iter().any(|x| x.block.is_some());
    println!();
    println!("{}chord            tension  gravity-default", if is_song { "loop  block  " } else { "loop  " });
    for x in &r.rows {
        let blk = match (is_song, x.block) {
            (false, _) => String::new(),
            (true, Some(b)) => format!("{b:>3}{}   ", if x.seam { "⇢" } else { " " }),
            (true, None) => "  —    ".into(),
        };
        let t = x.tension.map_or("-".to_string(), |t| format!("{t:.2}"));
        println!("{:5} {blk}{:<16} {t:<7}  {}", x.loop_id, x.chord.as_deref().unwrap_or("None"), x.default_chord.as_deref().unwrap_or("None"));
    }
    match &r.summary {
        None => println!("\nno harmonic moves — the Orrery found nothing to move (no `chord` voices?), or the run is shorter than a phrase"),
        Some(s) => {
            let d = s.distinct.iter().map(|(c, n)| format!("'{c}': {n}")).collect::<Vec<_>>().join(", ");
            println!("\ndistinct chords: {{{d}}}");
            println!("tension min/mean/max = {:.2} / {:.2} / {:.2};  longest excursion above 0.2 = {} loop(s)", s.tension_min, s.tension_mean, s.tension_max, s.longest_excursion);
            println!("most-visited chord: {} ({}/{} loops)", s.most_visited.0, s.most_visited.1, s.moves);
            println!(
                "{}",
                if s.tamed {
                    "TAMED: the voyage stays near home (mean ≤ 0.12, max ≤ 0.45, excursions ≤ 2 loops)."
                } else {
                    "ROAMING: the voyage strays — thicken the context floor or re-home the chord role."
                }
            );
        }
    }
    if is_song {
        println!("\n== per block (the voyage restarts from home at every block; ⇢ = the seam loop) ==");
        for b in &r.blocks {
            match &b.summary {
                None => println!("  block {}: {} loop(s), no moves", b.block, b.loops),
                Some(s) => println!(
                    "  block {}: {:3} loops · {} chords · tension mean {:.2} max {:.2} · excursion {} · {}{} · opens {}",
                    b.block,
                    b.loops,
                    s.distinct.len(),
                    s.tension_mean,
                    s.tension_max,
                    s.longest_excursion,
                    if s.tamed { "tamed" } else { "ROAMING" },
                    if b.indicative { " (few moves — indicative)" } else { "" },
                    b.opens.join(" → ")
                ),
            }
        }
        for n in &r.recaps {
            println!("  {n}");
        }
    }
    println!();
    r.findings.print();
    println!("VERDICT: {}", r.verdict);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moves(ts: &[f64]) -> Vec<(String, f64)> {
        ts.iter().map(|&t| (if t > 0.2 { "IV".to_string() } else { "I".to_string() }, t)).collect()
    }

    #[test]
    fn a_voyage_that_stays_home_is_tamed_and_one_that_never_comes_back_roams() {
        let home = summarize(&moves(&[0.0, 0.0, 0.3, 0.0, 0.1, 0.0, 0.0, 0.25, 0.3, 0.0])).unwrap();
        assert!(home.tamed, "{home:?}");
        assert_eq!((home.longest_excursion, home.most_visited.clone(), home.distinct.len()), (2, ("I".to_string(), 7), 2));
        // Away from loop 2 on, never back under 0.2.
        let gone = summarize(&moves(&[0.0, 0.0, 0.3, 0.35, 0.3, 0.4, 0.3, 0.35, 0.3, 0.4])).unwrap();
        assert!(!gone.tamed && gone.longest_excursion == 8, "{gone:?}");
        // The mean alone can roam it, and so can one spike over 0.45.
        assert!(!summarize(&moves(&[0.15, 0.15, 0.15, 0.15])).unwrap().tamed);
        assert!(!summarize(&moves(&[0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0])).unwrap().tamed);
        assert!(summarize(&[]).is_none());
    }

    #[test]
    fn the_most_visited_chord_breaks_ties_by_first_visit() {
        let m: Vec<(String, f64)> = ["V", "I", "V", "I"].iter().map(|c| (c.to_string(), 0.0)).collect();
        assert_eq!(summarize(&m).unwrap().most_visited, ("V".to_string(), 2));
    }
}
