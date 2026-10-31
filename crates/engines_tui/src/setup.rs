//! Single source of truth for the Setup screen's field list: identity, render label,
//! `--midi` gating, and — critically — the ORDER. `next_field` (focus cycling, app.rs) and
//! `draw_setup` (rendering, ui.rs) both iterate [`visible_fields`], so Tab focus and the
//! on-screen row order can never drift apart (they used to be three hand-synced lists).
//!
//! Adding a selector = one row here + one arm in `App::cycle_setup` + one value arm in
//! `ui::draw_setup`'s match (both matches are exhaustive, so the compiler walks you there).

use crate::app::SetupField;

pub struct FieldSpec {
    pub id: SetupField,
    /// Row label, padded to the Setup screen's 9-char label column.
    pub label: &'static str,
    /// Present only when `--midi` is on (ADR 0026 — these pair with MIDI-out).
    pub midi_gated: bool,
}

/// Every Setup field, in on-screen and Tab-cycle order.
pub const SETUP_FIELDS: &[FieldSpec] = &[
    FieldSpec { id: SetupField::Doc, label: "Doc      ", midi_gated: false },
    FieldSpec { id: SetupField::Optimize, label: "Optimize ", midi_gated: false },
    FieldSpec { id: SetupField::Orrery, label: "Orrery   ", midi_gated: false },
    FieldSpec { id: SetupField::ReturnDevice, label: "Return   ", midi_gated: true },
    FieldSpec { id: SetupField::MidiDither, label: "MIDIdith ", midi_gated: true },
    FieldSpec { id: SetupField::Output, label: "Output   ", midi_gated: false },
];

/// The fields shown/cycled for this session, in order.
pub fn visible_fields(midi: bool) -> impl Iterator<Item = &'static FieldSpec> {
    SETUP_FIELDS.iter().filter(move |f| midi || !f.midi_gated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_every_field_exactly_once() {
        // Compile-time nudge: a new SetupField variant fails this match until handled —
        // and the runtime check below then fails until it gets a SETUP_FIELDS row.
        for f in SETUP_FIELDS {
            match f.id {
                SetupField::Doc
                | SetupField::Optimize
                | SetupField::Orrery
                | SetupField::ReturnDevice
                | SetupField::MidiDither
                | SetupField::Output => {}
            }
        }
        let all = [
            SetupField::Doc,
            SetupField::Optimize,
            SetupField::Orrery,
            SetupField::ReturnDevice,
            SetupField::MidiDither,
            SetupField::Output,
        ];
        for id in all {
            assert_eq!(SETUP_FIELDS.iter().filter(|f| f.id == id).count(), 1, "{id:?} must appear exactly once");
        }
        assert_eq!(SETUP_FIELDS.len(), all.len());
        // The midi-gated pair is exactly Return + MIDIdith (ADR 0026).
        let gated: Vec<_> = SETUP_FIELDS.iter().filter(|f| f.midi_gated).map(|f| f.id).collect();
        assert_eq!(gated, [SetupField::ReturnDevice, SetupField::MidiDither]);
    }
}
