//! The controller's **movement axis** (Orrery, ADR 0014–0019): role binding, harmonic
//! stepping, leader-led arrangement, drops, and track returns. Split from `controller.rs`
//! (pure code motion) — these are `impl Controller` methods; state (`OrreryState` etc.)
//! stays with the struct in the parent module.

use super::*;

impl ModSnapshot {
    /// The same authored Orrery-writable content (the doc types have no `PartialEq`; compared as
    /// JSON, which is exact for what the doc can express).
    fn same(&self, other: &ModSnapshot) -> bool {
        fn j<T: serde::Serialize>(v: &T) -> DocValue {
            crate::doc_value::to_doc_value(v).unwrap_or_default()
        }
        self.wave == other.wave
            && self.phrase == other.phrase
            && j(&self.steps) == j(&other.steps)
            && j(&self.pitch_walk) == j(&other.pitch_walk)
            && j(&self.generator) == j(&other.generator)
    }
}

/// What a running voyage stands on: the key, and which modulators carry the chord and the bass
/// (their ids, type and note source). A live edit that keeps it keeps the voyage.
fn same_footing(old: &LoopDoc, old_cfg: &OrreryConfig, new: &LoopDoc, new_cfg: &OrreryConfig) -> bool {
    fn carriers(c: &OrreryConfig) -> Vec<String> {
        let mut ids = vec![c.chord_mod.clone(), c.bass_mod.clone()];
        ids.extend(c.extra_chord_mods.iter().cloned());
        ids
    }
    fn shape(d: &LoopDoc, id: &str) -> Option<(String, String, bool, bool)> {
        d.modulators.get(id).map(|m| (m.kind.clone(), m.output.clone(), m.generator.is_some(), m.phrase.is_some()))
    }
    let ids = carriers(new_cfg);
    old.harmony.root == new.harmony.root
        && old.harmony.mode == new.harmony.mode
        && carriers(old_cfg) == ids
        && ids.iter().all(|id| shape(old, id) == shape(new, id))
}

impl Controller {
    /// Start a voyage from home: the Orrery turning on, its binding changing, or a live edit that
    /// moved its footing (a new key, a restructured chord/bass).
    pub(super) fn start_voyage(&mut self) {
        let cfg = self.orrery_config();
        let scale = Scale::from_harmony(&self.base.harmony.root, &self.base.harmony.mode);
        let pref = self.harmonic_pref();
        let fixed = self.fixed_pitch_context(&cfg);
        let arp_len = self.base.modulators.get(&cfg.arp_mod).map(|m| m.length).unwrap_or(16);
        let voyager = Voyager::new(scale, &cfg.pad_ids, &pref, cfg.mel_len, self.cfg.seed);
        write_phrase(&mut self.base, &voyager.plan(), &cfg);
        enforce_ranges(&mut self.base, &cfg.written_modulators());
        // Optional Arranger tuning from `Movement.arrange` (Phase 6d); absent ⇒ built-in defaults.
        let mut arranger = Arranger::new(arp_len);
        if let Some(band) = self.control.as_ref().and_then(|c| c.movement.arrange.as_ref()).and_then(|a| a.arp_density.as_ref()) {
            arranger = arranger.with_density_band(band.min, band.max);
        }
        self.orrery = Some(OrreryState {
            voyager,
            gravity: Gravity::default(),
            pref,
            cfg,
            fixed,
            vc_recent: Vec::new(),
            track_home: BTreeMap::new(),
            last_leader: None,
            last_drop: None,
            last_returned_home: false,
            arranger,
            last_plan: ArrangePlan::default(),
            loops_seen: 0,
        });
    }

    /// After a live edit put a new authored doc in `self.base` / `self.home` (`old_base` is what
    /// played before it, `old_home` its authored home, `running` its voyage): the Orrery keeps
    /// wandering through the edit — same voyage, same phrase clock — unless the edit moved its
    /// footing (the key, the chord/bass modulators), which starts it again from home (the approved
    /// plan's rule; ADR 0035 amendment). Each modulator the edit left alone keeps what the voyage
    /// wrote into it; one the edit touched plays as written until the next phrase rewrites it.
    pub(super) fn follow_edit(&mut self, old_base: &LoopDoc, old_home: &HomeSnapshot, running: Option<OrreryState>) {
        if !self.is_orrery() {
            return;
        }
        let Some(mut orr) = running else {
            if self.orrery.is_none() {
                self.start_voyage();
            }
            return;
        };
        let cfg = self.orrery_config();
        if !same_footing(old_base, &orr.cfg, &self.base, &cfg) {
            if self.orrery.is_none() {
                self.start_voyage(); // (a rebuilt controller already started one)
            }
            return;
        }
        for (id, m) in self.base.modulators.iter_mut() {
            let kept = match (old_home.modulators.get(id), self.home.modulators.get(id), old_base.modulators.get(id)) {
                (Some(was), Some(now), Some(playing)) if was.same(now) => playing,
                _ => {
                    // Touched (or new): as written — undoing any home write a rebuild made.
                    if let Some(snap) = self.home.modulators.get(id) {
                        m.steps = snap.steps.clone();
                        m.pitch_walk = snap.pitch_walk.clone();
                        m.generator = snap.generator.clone();
                        m.wave = snap.wave.clone();
                        m.phrase = snap.phrase.clone();
                    }
                    continue;
                }
            };
            m.steps = kept.steps.clone();
            m.pitch_walk = kept.pitch_walk.clone();
            m.generator = kept.generator.clone();
            m.wave = kept.wave.clone();
            m.phrase = kept.phrase.clone();
        }
        orr.fixed = self.fixed_pitch_context(&cfg);
        orr.pref = self.harmonic_pref();
        orr.cfg = cfg;
        self.orrery = Some(orr);
    }

    /// Build the Orrery layer config from the loop doc + movement axis. Phase 4 resolves each role
    /// by **`modulators.<id>.orrery_role`** first, falling back to `movement.bind.<role>` ids, then
    /// the demo defaults — so Orrery binds by *role*, not by hardcoded demo ids. `oneshot` modulators
    /// (accent surface) and read-only `context`/`orrery_disable` modulators (the fixed-pitch floor)
    /// are resolved here too.
    pub(super) fn orrery_config(&self) -> OrreryConfig {
        let mut cfg = OrreryConfig::default();
        let chord_mods = self.role_mods("chord");
        let arp_mods = self.role_mods("arp");
        if let Some(c) = self.control.as_ref() {
            let bind = &c.movement.bind;
            cfg.chord_mod = chord_mods.first().cloned().unwrap_or_else(|| bind.chord.clone());
            cfg.bass_mod = self.role_mod("bass").unwrap_or_else(|| bind.bass.clone());
            cfg.arp_mod = arp_mods.first().cloned().unwrap_or_else(|| bind.arp.clone());
            cfg.oneshot_mods = self.resolve_oneshots(&bind.oneshot);
            cfg.phrase_loops = c.movement.phrase_loops.max(1);
        } else {
            cfg.chord_mod = chord_mods.first().cloned().unwrap_or(cfg.chord_mod);
            cfg.bass_mod = self.role_mod("bass").unwrap_or(cfg.bass_mod);
            cfg.arp_mod = arp_mods.first().cloned().unwrap_or(cfg.arp_mod);
            cfg.oneshot_mods = self.resolve_oneshots(&[]);
        }
        // Additional same-role tracks beyond the primary (Phase 6c multi-track chords/arps).
        cfg.extra_chord_mods = chord_mods.into_iter().skip(1).collect();
        cfg.extra_arp_mods = arp_mods.into_iter().skip(1).collect();
        cfg.context_mods = self.context_mods();
        cfg
    }

    /// All note modulators (by sorted id) carrying `orrery_role == role`.
    pub(super) fn role_mods(&self, role: &str) -> Vec<String> {
        self.base
            .modulators
            .iter()
            .filter(|(_, m)| m.orrery_role.as_deref() == Some(role))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// First note modulator (by sorted id) carrying `orrery_role == role`, if any.
    pub(super) fn role_mod(&self, role: &str) -> Option<String> {
        self.role_mods(role).into_iter().next()
    }

    /// One-shot accent modulators: every `orrery_role: oneshot` modulator (by sorted id); else the
    /// `bind.oneshot` ids that actually name an existing modulator (legacy instrument names drop out).
    pub(super) fn resolve_oneshots(&self, bind_oneshot: &[String]) -> Vec<String> {
        let by_role: Vec<String> = self
            .base
            .modulators
            .iter()
            .filter(|(_, m)| m.orrery_role.as_deref() == Some("oneshot"))
            .map(|(id, _)| id.clone())
            .collect();
        if !by_role.is_empty() {
            return by_role;
        }
        bind_oneshot.iter().filter(|id| self.base.modulators.contains_key(*id)).cloned().collect()
    }

    /// Read-only fixed-pitch context: note modulators flagged `orrery_disable` or `orrery_role:
    /// context`. Orrery never rewrites these; their sounding pitches form the harmonic floor (WS4.2).
    pub(super) fn context_mods(&self) -> Vec<String> {
        self.base
            .modulators
            .iter()
            .filter(|(_, m)| m.output == "note" && (m.orrery_disable || m.orrery_role.as_deref() == Some("context")))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Restore the full authored Orrery footprint (`steps` + `pitch_walk` + `generator` + LFO `wave`)
    /// of the given modulators from the [`HomeSnapshot`] (Phase 5b; widened in Phase 6a). Only the
    /// movement-axis leaves are touched — timbre drift / overrides on the operating point are
    /// preserved (they live on the optimize axis the seam keeps disjoint). Used both for a single
    /// returned-home *track* and (via [`restore_home`](Self::restore_home)) for the Orrery-off footprint.
    pub(super) fn restore_tracks(&mut self, ids: &[String]) {
        for id in ids {
            if let Some(snap) = self.home.modulators.get(id).cloned() {
                if let Some(m) = self.base.modulators.get_mut(id) {
                    m.steps = snap.steps;
                    m.pitch_walk = snap.pitch_walk;
                    m.generator = snap.generator;
                    m.wave = snap.wave;
                    m.phrase = snap.phrase;
                }
            }
        }
    }

    /// Revert the **entire** Orrery footprint to the authored home (Orrery-off). Because the axis seam
    /// (ADR 0017) makes the optimize axis disjoint from `modulators.*`, restoring every captured
    /// modulator is exactly "undo everything the voyage did" — a total return home (chord/bass/arp/
    /// one-shot note content, euclid generators, pitch-walk order, and LFO wave-shapes).
    pub(super) fn restore_home(&mut self) {
        let ids: Vec<String> = self.home.modulators.keys().cloned().collect();
        self.restore_tracks(&ids);
    }

    /// The authored ("home") sounding pitch-classes of a modulator — from the [`HomeSnapshot`], so it
    /// is the *original authored* content, not whatever the voyage has since written (Phase 5b).
    pub(super) fn authored_pcs(&self, mod_id: &str) -> Vec<Pc> {
        let root = pc_of_root(&self.base.harmony.root);
        let mut pcs: Vec<Pc> = Vec::new();
        if let Some(snap) = self.home.modulators.get(mod_id) {
            let offs = snap
                .steps
                .iter()
                .flat_map(|s| s.voicing.values().map(|nv| nv.offset))
                .chain(snap.pitch_walk.iter().flat_map(|pw| pw.pool.iter().copied()));
            for off in offs {
                let pc = Pc::from_offset(root, off.round() as i32);
                if !pcs.contains(&pc) {
                    pcs.push(pc);
                }
            }
        }
        pcs
    }

    /// Per-phrase organic return draws (Phase 5b, `home == per_modulator`): for each bound non-chord
    /// track, a seeded Bernoulli against its growing [`return_hazard`]. Returns the roles that fired
    /// (they rest to authored home this phrase). Deterministic + staggered (per-track seed sub-stream).
    pub(super) fn draw_track_returns(&self, orr: &OrreryState, phrase: u64, return_scale: f32) -> Vec<String> {
        let mut fired = Vec::new();
        let tension = orr.voyager.current_tension();
        for (role, haz) in [("bass", HAZ_BASS), ("arp", HAZ_ARP), ("oneshot", HAZ_ONESHOT)] {
            if orr.cfg.track_modulators(role).iter().all(|id| !self.base.modulators.contains_key(id)) {
                continue; // no bound modulator for this role
            }
            let n = *orr.track_home.get(role).unwrap_or(&0);
            if hash_to_unit(hash_site(self.cfg.seed, "return", phrase, role)) < return_hazard(haz, n, tension) * return_scale {
                fired.push(role.to_string());
            }
        }
        fired
    }

    /// The leader among fired non-chord tracks: precedence **bass > arp**; one-hits never lead (a
    /// sparse accent shouldn't re-pick the chord — they rest as followers). Phase 5b.
    pub(super) fn pick_leader(fired: &[String]) -> Option<String> {
        ["bass", "arp"].into_iter().find(|r| fired.iter().any(|f| f == r)).map(str::to_string)
    }

    /// Build the chord constraint for a non-chord leader's authored notes (Phase 5b). An arp leader
    /// harmonizes its melody (`LeaderFit::Melody`); a bass leader pedals freely above scale-degree 1/5
    /// (`None`) or otherwise requires the bass to be a chord tone (`LeaderFit::BassChordTone`).
    pub(super) fn leader_fit(&self, orr: &OrreryState, role: &str) -> Option<LeaderFit> {
        match role {
            "arp" => {
                let pcs = self.authored_pcs(&orr.cfg.arp_mod);
                (!pcs.is_empty()).then_some(LeaderFit::Melody(pcs))
            }
            "bass" => {
                let b = *self.authored_pcs(&orr.cfg.bass_mod).first()?;
                let scale = Scale::from_harmony(&self.base.harmony.root, &self.base.harmony.mode);
                match scale.degree_of(b) {
                    Some(0) | Some(4) => None, // tonic/dominant pedal — free chord above the held bass
                    _ => Some(LeaderFit::BassChordTone(b)),
                }
            }
            _ => None,
        }
    }

    /// Whether the chord's own organic return-home fires this phrase (Phase 5b; tension-driven in
    /// Phase 6a). Drives the cadence-aware return in **both** home scopes — a per-track resolution
    /// under `per_modulator` and the whole-voyage organic return under `global`. The *cadence* gate
    /// (only resolve when poised) lives in `Voyager::advance`; this is just the hazard draw.
    pub(super) fn chord_return_fires(&self, orr: &OrreryState, phrase: u64, return_scale: f32) -> bool {
        let n = orr.voyager.loops_since_home();
        let tension = orr.voyager.current_tension();
        hash_to_unit(hash_site(self.cfg.seed, "return", phrase, "chord")) < return_hazard(HAZ_CHORD, n, tension) * return_scale
    }

    /// Phase 6c narrative drop: pick a prominent role (`chord`/`arp`) to silence this phrase, or
    /// `None`. Only at a tension peak (high energy, away from home, no leader, no resolution pending) —
    /// the producer's "thin before the drop"; the voice re-enters next phrase. Seeded ⇒ deterministic;
    /// never drops a track that just returned home, and never `instruments.enabled` (the seam).
    pub(super) fn pick_drop(&self, orr: &OrreryState, phrase: u64, energy: f32, fired: &[String], leader: &Option<String>, want_return: bool, breach: bool) -> Option<&'static str> {
        if breach || want_return || leader.is_some() || orr.voyager.loops_since_home() == 0 || energy < DROP_ENERGY_MIN {
            return None;
        }
        let haz = (DROP_P0 + DROP_K * (energy - DROP_ENERGY_MIN)).clamp(0.0, DROP_MAX);
        if hash_to_unit(hash_site(self.cfg.seed, "drop", phrase, "fire")) >= haz {
            return None;
        }
        let cands: Vec<&'static str> = ["chord", "arp"]
            .into_iter()
            .filter(|role| {
                !fired.iter().any(|r| r == role)
                    && orr.cfg.track_modulators(role).iter().any(|id| self.base.modulators.contains_key(id))
            })
            .collect();
        if cands.is_empty() {
            return None;
        }
        let pick = (hash_to_unit(hash_site(self.cfg.seed, "drop", phrase, "which")) * cands.len() as f32) as usize % cands.len();
        Some(cands[pick])
    }

    /// The Orrery harmonic preference (the "solar wind"): the built-in adventure, the pad register
    /// following the chord track's **authored** octave (ADR 0041), and the movement axis's
    /// `tension_target` anchoring the tension band (without a control, the default target).
    pub(super) fn harmonic_pref(&self) -> HarmonicPreference {
        let mut pref = HarmonicPreference::default();
        if let Some(c) = self.control.as_ref() {
            pref.tension_target = c.movement.tension_target.clamp(0.05, 0.95);
        }
        let chord_mod = self.orrery_config().chord_mod;
        let home: Vec<f32> = self
            .home
            .modulators
            .get(&chord_mod)
            .map(|snap| snap.steps.iter().flat_map(|s| s.voicing.values().map(|nv| nv.offset)).collect())
            .unwrap_or_default();
        pref.pad_center = pad_center_for(&home);
        pref
    }

    /// The sounding pitch-classes of the `context_mods` (held drones) — the fixed-pitch context the
    /// voyage is biased to stay consonant with (WS4.2). Harvests both authored `voicing` offsets and
    /// `pitch_walk` pools; de-duplicated. Computed once when the voyage is (re)built.
    pub(super) fn fixed_pitch_context(&self, cfg: &OrreryConfig) -> Vec<Pc> {
        let root = pc_of_root(&self.base.harmony.root);
        let mut pcs: Vec<Pc> = Vec::new();
        for id in &cfg.context_mods {
            let Some(m) = self.base.modulators.get(id) else { continue };
            let offsets = m
                .steps
                .iter()
                .flat_map(|s| s.voicing.values().map(|nv| nv.offset))
                .chain(m.pitch_walk.iter().flat_map(|pw| pw.pool.iter().copied()));
            for off in offsets {
                let pc = Pc::from_offset(root, off.round() as i32);
                if !pcs.contains(&pc) {
                    pcs.push(pc);
                }
            }
        }
        pcs
    }

    /// One Orrery slow-layer step: on a phrase boundary, advance the voyage, rewrite the
    /// operating-point's note modulators (chord/bass/melody) and arrangement, and return the
    /// move. Between boundaries the harmony is held (the engine glides on phrase changes).
    pub(super) fn orrery_step(&mut self) -> Option<HarmonicMove> {
        let mut orr = self.orrery.take()?;
        let phrase_loops = orr.cfg.phrase_loops.max(1);
        let mut mv = None;
        if orr.loops_seen > 0 && orr.loops_seen % phrase_loops == 0 {
            let phrase = orr.loops_seen / phrase_loops;
            let (tmax, scope) = self
                .control
                .as_ref()
                .map(|c| (c.movement.tension_max, c.movement.home))
                .unwrap_or((1.0, HomeScope::Global));
            let per_mod = matches!(scope, HomeScope::PerModulator);
            // Optional global return-hazard scale from `Movement.arrange` (Phase 6d; default 1.0 ⇒
            // byte-identical). Applied on top of the Arranger's build/release `return_scale`.
            let haz_scale =
                self.control.as_ref().and_then(|c| c.movement.arrange.as_ref()).map(|a| a.return_hazard_scale).unwrap_or(1.0);

            // The Arranger (Phase 6b): advance the slow energy arc and read this phrase's plan —
            // register octaves, arp density, accent role, and the build/release overlay. The energy
            // overlay breathes the tension target and scales the return hazards (the producer "form").
            let scale = Scale::from_harmony(&self.base.harmony.root, &self.base.harmony.mode);
            let pad_center = orr.pref.pad_center;
            let arp_center = scale.degree_semitone(orr.pref.mel_register);
            let arr = orr.arranger.advance(phrase, orr.voyager.current_tension(), self.cfg.seed, pad_center, arp_center);
            orr.last_plan = arr;
            let mut pref = orr.pref;
            pref.tension_target = (orr.pref.tension_target + arr.tension_bias).clamp(0.05, 0.95);

            // Hard ceiling (WS5.4): a `tension_max` breach forces a return home this phrase — the tonic
            // is the calmest node, so it self-arms (natural hysteresis). `home == global` resets the
            // whole voyage; `per_modulator` resolves the harmony while the melodic surface keeps roaming.
            let breach = orr.voyager.current_tension() > tmax && orr.voyager.loops_since_home() > 0;

            // Per-track organic returns (Phase 5b, per_modulator only). Each fired track rests to its
            // **authored** home; the top-precedence fired non-chord track (bass > arp; one-hits never
            // lead) becomes the **leader** the chord must fit. The chord's own return is cadence-aware
            // (gated inside `advance`) and yields to a non-chord leader. Hazards are scaled by the
            // Arranger's `return_scale` (build damps resolution; release invites it, ADR 0018).
            let fired = if per_mod && !breach { self.draw_track_returns(&orr, phrase, arr.return_scale * haz_scale) } else { Vec::new() };
            let leader = if per_mod { Self::pick_leader(&fired) } else { None };
            orr.last_leader = leader.clone();
            let leader_fit = leader.as_deref().and_then(|r| self.leader_fit(&orr, r));
            // Organic chord return — in **both** scopes (ADR 0017): a per-track resolution under
            // `per_modulator`, the whole-voyage organic return under `global` (on top of the hard
            // breach). Gated off when a non-chord leader is serving the arrangement this phrase.
            let want_return = !breach && leader.is_none() && self.chord_return_fires(&orr, phrase, arr.return_scale * haz_scale);
            let lsh_before = orr.voyager.loops_since_home();

            let m = if breach {
                for v in orr.track_home.values_mut() {
                    *v = 0;
                }
                orr.voyager.return_home(&pref, matches!(scope, HomeScope::Global), hash64(self.cfg.seed, phrase, 0x40E, 0))
            } else {
                let adv = AdvanceOpts { leader: leader_fit.as_ref(), want_return };
                orr.voyager.advance(&pref, &orr.gravity, &orr.fixed, &adv, hash64(self.cfg.seed, phrase, 0xA11CE, 0))
            };
            // Telemetry (Phase 6d): a homeward return *landed* this phrase — a per-track rest, or the
            // chord voyage reaching home (the breach snap or a resolved cadence; `want_return` is only
            // the hazard draw, so we check `loops_since_home` actually hit 0). Surfaced as `returning_home`.
            orr.last_returned_home = !fired.is_empty() || (lsh_before > 0 && orr.voyager.loops_since_home() == 0);

            // Realize the surfaces, then apply the Arranger: collision-aware register octaves, arp
            // density (euclid pulses), and differentiated accents (color tones, high register).
            let plan = orr.voyager.plan();
            write_phrase(&mut self.base, &plan, &orr.cfg);
            vary_arp_pool(&mut self.base, &orr.cfg, hash64(self.cfg.seed, phrase, 0x0A59, 0));
            // Phrase-armed tracks (ADR 0028): select + develop + chord-refit a sequence-pool
            // entry per phrase window; the Arranger's energy gates busy vs calm entries.
            // Before `apply_octaves` so the materialized copy register-moves with everything.
            select_and_refit_phrases(&mut self.base, &orr.cfg, &plan, scale.degrees(), arr.energy, hash64(self.cfg.seed, phrase, 0x0AD0, 0));
            apply_octaves(&mut self.base, &orr.cfg, arr.pad_octave, arr.bass_octave, arr.arp_octave);
            write_arp_density(&mut self.base, &orr.cfg, arr.arp_pulses);
            let accent_center = (12 + 12 * arr.accent_octave).clamp(13, 31);
            write_oneshots(&mut self.base, &plan, &orr.cfg, accent_center, arr.accent_color, hash64(self.cfg.seed, phrase, 0x05E7, 0));

            // Rest each fired track to its authored home (overwriting its chord-derived write). The
            // chord was chosen to fit the leader, so followers stay consonant with it (Phase 5b).
            for role in &fired {
                let ids = orr.cfg.track_modulators(role);
                self.restore_tracks(&ids);
            }
            // Per-track home clocks: reset the rested tracks (and every track on a global breach),
            // increment the rest. Only the bound roles are clocked.
            if per_mod {
                for role in ["bass", "arp", "oneshot"] {
                    if orr.cfg.track_modulators(role).iter().all(|id| !self.base.modulators.contains_key(id)) {
                        continue;
                    }
                    let e = orr.track_home.entry(role.to_string()).or_insert(0);
                    *e = if breach || fired.iter().any(|r| r == role) { 0 } else { *e + 1 };
                }
            }

            // Phase 6c narrative drop: at a tension peak, thin a prominent voice (release the chord or
            // silence the arp) so the texture breaks down before the return lands; it re-enters on the
            // next phrase's normal write. Never touches `enabled` (the seam) — a modulator move only.
            let dropped = self.pick_drop(&orr, phrase, arr.energy, &fired, &leader, want_return, breach);
            if let Some(role) = dropped {
                let ids = orr.cfg.track_modulators(role);
                drop_surface(&mut self.base, &ids);
            }
            orr.last_drop = dropped.map(str::to_string);
            // Each modulator's `orrery_note_min..=max` bounds everything the Orrery wrote this phrase,
            // a track it rested at its authored home included (ADR 0042).
            enforce_ranges(&mut self.base, &orr.cfg.written_modulators());

            // Track voice changes every phrase (for telemetry + the governor's moving average).
            orr.vc_recent.push(orr.voyager.last_voice_changes());
            if orr.vc_recent.len() > VC_WINDOW {
                orr.vc_recent.remove(0);
            }
            // Voice-change governor (WS5.3): hold the moving average inside `voice_changes_per_loop`
            // by nudging common-tone retention. Hysteresis — only act when the average leaves the
            // band — keeps it from oscillating. Inert (retention stays put) when no band is set.
            if let Some(band) = self.control.as_ref().and_then(|c| c.movement.voice_changes_per_loop) {
                let avg = orr.vc_recent.iter().sum::<u32>() as f32 / orr.vc_recent.len() as f32;
                let r = orr.voyager.retain();
                let new_r = if avg > band.max {
                    (r + 1).min(orr.cfg.pad_ids.len())
                } else if avg < band.min {
                    r.saturating_sub(1)
                } else {
                    r
                };
                orr.voyager.set_retain(new_r);
            }
            mv = Some(m);
        }
        orr.loops_seen += 1;
        self.orrery = Some(orr);
        mv
    }
}
