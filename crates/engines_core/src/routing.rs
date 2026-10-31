//! The piece's **routing map** (ADR 0044): which device stereo pair each audio source plays
//! on, so the live player's output (`engines-live --route-sources`) and the Ableton Live set's tracks
//! (`engines-als`) agree. Stored as `pieces/<slug>/ableton/<slug>.routing.json`.
//!
//! Assignments are **sticky**: a source keeps its pair across rebuilds (its Live track listens
//! there), a source that leaves the doc is marked `stale` and keeps its pair reserved (its track is
//! never deleted), and a new source takes the lowest free pair. One pair is the **overflow**: it
//! carries whatever no slot does (see [`SourceRouting`]) and gets its own track; when the device is
//! full it shares the last pair.
//!
//! This is the open part of the map: the pairs. Every other field — what the Live set builder
//! records (track ids, the template, the mode, faders, colours) and a slot's `midi` (the MIDI mirror,
//! ADR 0048) — is kept as it was read and written back unchanged, at every level, so a tool that
//! doesn't know a field never erases it.

use crate::engine::SourceKey;
use crate::piece::Piece;
use crate::session::SourceRouting;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// The map's format tag.
pub const SCHEMA: &str = "engines-routing/1";
/// The tag of a map written before the rename to Engines: the same format, still read; the
/// next write gives it the new tag.
pub const LEGACY_SCHEMA: &str = "sme-routing/1";

/// Where a piece's routing map lives.
pub fn path(piece: &Piece) -> PathBuf {
    piece.ableton_dir().join(format!("{}.routing.json", piece.slug))
}

/// A piece's routing map.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutingMap {
    pub schema: String,
    pub piece: String,
    /// Device channels the map was built for (BlackHole 64ch ⇒ 64).
    pub channels: usize,
    /// Slotted sources, by channel.
    pub slots: Vec<Slot>,
    pub overflow: Overflow,
    /// Sources with no pair of their own (they play on the overflow), as `inst:x` / `bus:x`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrouted: Vec<String>,
    /// Stale slots a new source took over (their Live tracks stay, now unrouted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<Slot>,
    /// Every other field (the Live set builder's): kept and written back unchanged.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One source on one stereo pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    /// The pair's first device channel, 1-based and odd (`3` ⇒ channels 3/4) — the ADR 0027
    /// `out_channel` convention, and what Live shows as the input "3/4".
    pub channel: usize,
    /// `inst:<id>` or `bus:<id>` (`SourceKey`'s display form).
    pub source: String,
    /// The track / stem name (`stems::assign_names`).
    pub name: String,
    /// The source has left the doc; the pair stays reserved for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    /// Every other field (its Live track's id, its `midi`): kept with the slot, written back unchanged.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Slot {
    /// A new slot: nothing recorded about it yet.
    pub fn new(channel: usize, source: String, name: String) -> Slot {
        Slot { channel, source, name, stale: false, extra: Map::new() }
    }
}

/// The overflow pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overflow {
    pub channel: usize,
    /// Set when the device is full: the source whose pair the overflow shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_with: Option<String>,
    /// Every other field (its Live track's id): kept while the overflow stays on its pair.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// How [`assign`] treats sources the previous map doesn't slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// `engines-als`: give them pairs (the set gets tracks for them).
    Grow,
    /// `engines-live`: leave the slots as built (the open set's tracks); new sources ride the overflow.
    Frozen,
}

/// The pair index (0-based) of a 1-based odd first channel.
pub fn pair_of(channel: usize) -> usize {
    channel.saturating_sub(1) / 2
}

/// The 1-based first channel of a 0-based pair.
pub fn channel_of(pair: usize) -> usize {
    2 * pair + 1
}

/// `3` → `"3/4"`.
pub fn pair_label(channel: usize) -> String {
    format!("{}/{}", channel, channel + 1)
}

/// `inst:x` / `bus:x` → the source key (the inverse of `SourceKey`'s display).
pub fn parse_source(s: &str) -> Option<SourceKey> {
    match s.split_once(':')? {
        ("inst", id) if !id.is_empty() => Some(SourceKey::Inst(id.to_string())),
        ("bus", id) if !id.is_empty() => Some(SourceKey::Bus(id.to_string())),
        _ => None,
    }
}

/// Assign `sources` (in the order their tracks should appear: the doc's authored order, instruments
/// then buses; each with its track name) to the pairs of a `channels`-wide device, keeping `prev`'s
/// assignments and every field of `prev`'s this code doesn't know. Returns the map and notes for the
/// user.
pub fn assign(piece: &str, prev: Option<&RoutingMap>, sources: &[(SourceKey, String)], channels: usize, policy: Policy) -> Result<(RoutingMap, Vec<String>), String> {
    if channels < 4 || !channels.is_multiple_of(2) {
        return Err(format!("a routed device needs an even channel count of at least 4 (one source + the overflow), not {channels}"));
    }
    let pairs = channels / 2;
    let mut notes = Vec::new();
    let present: Vec<String> = sources.iter().map(|(k, _)| k.to_string()).collect();
    let name_of = |src: &str| sources.iter().find(|(k, _)| k.to_string() == src).map(|(_, n)| n.clone());

    // Previous slots that still fit the device; a narrower device re-assigns the rest.
    let mut slots: Vec<Slot> = Vec::new();
    let mut retired = prev.map(|p| p.retired.clone()).unwrap_or_default();
    for s in prev.map(|p| p.slots.as_slice()).unwrap_or_default() {
        if pair_of(s.channel) >= pairs {
            notes.push(format!("{} was on {}, past this {channels}-channel device — reassigned", s.source, pair_label(s.channel)));
            continue;
        }
        let mut s = s.clone();
        s.stale = !present.contains(&s.source);
        if let Some(n) = name_of(&s.source) {
            s.name = n;
        }
        slots.push(s);
    }
    let taken = |slots: &[Slot], pair: usize| slots.iter().any(|s| pair_of(s.channel) == pair);
    let prev_overflow = prev.map(|p| pair_of(p.overflow.channel)).filter(|&p| p < pairs && !taken(&slots, p));

    let mut unrouted = Vec::new();
    for (key, name) in sources {
        let src = key.to_string();
        if slots.iter().any(|s| s.source == src) {
            continue;
        }
        if policy == Policy::Frozen {
            unrouted.push(src);
            continue;
        }
        let free = (0..pairs).find(|&p| !taken(&slots, p) && Some(p) != prev_overflow);
        if let Some(p) = free {
            slots.push(Slot::new(channel_of(p), src, name.clone()));
        } else if let Some(i) = slots.iter().enumerate().filter(|(_, s)| s.stale).min_by_key(|(_, s)| s.channel).map(|(i, _)| i) {
            let channel = slots[i].channel;
            let old = std::mem::replace(&mut slots[i], Slot::new(channel, src.clone(), name.clone()));
            notes.push(format!("{src} took {} from the removed {}", pair_label(old.channel), old.source));
            retired.push(old);
        } else {
            notes.push(format!("{src} has no free pair — it plays on the overflow track"));
            unrouted.push(src);
        }
    }
    slots.sort_by_key(|s| s.channel);

    let overflow = match prev_overflow.or_else(|| (0..pairs).find(|&p| !taken(&slots, p))) {
        Some(p) => {
            // What was recorded about the overflow (its track) stays while it stays on its pair.
            let extra = prev.filter(|pm| pair_of(pm.overflow.channel) == p).map(|pm| pm.overflow.extra.clone()).unwrap_or_default();
            Overflow { channel: channel_of(p), shared_with: None, extra }
        }
        None => {
            let last = slots.iter().find(|s| pair_of(s.channel) == pairs - 1).map(|s| s.source.clone());
            notes.push(format!("the device is full: the overflow shares {} with {}", pair_label(channel_of(pairs - 1)), last.as_deref().unwrap_or("nothing")));
            Overflow { channel: channel_of(pairs - 1), shared_with: last, extra: Map::new() }
        }
    };
    let extra = prev.map(|p| p.extra.clone()).unwrap_or_default();
    let map = RoutingMap { schema: SCHEMA.into(), piece: piece.into(), channels, slots, overflow, unrouted, retired, extra };
    Ok((map, notes))
}

/// A doc's audio sources (the ADR 0030 layout) in the order their tracks should appear — the
/// instruments in `instruments`' order (the ids as the doc's text writes them), then the buses in
/// `buses`' (a backfilled default bus last) — each with its track / stem name
/// (`stems::assign_names`, so tracks match stem files). The orders come from the doc's text (the
/// CST's `member_order`); a source missing from them goes after the others of its kind.
#[cfg(feature = "stems")]
pub fn doc_sources(doc: &crate::loop_doc::LoopDoc, instruments: &[String], buses: &[String]) -> Vec<(SourceKey, String)> {
    let layout = crate::engine::source_layout(doc);
    let names = crate::stems::assign_names(&layout);
    let named: Vec<(SourceKey, String)> = layout.into_iter().zip(names).collect();
    let mut out: Vec<(SourceKey, String)> = Vec::with_capacity(named.len());
    let authored = instruments.iter().cloned().map(SourceKey::Inst).chain(buses.iter().cloned().map(SourceKey::Bus));
    for key in authored {
        if let Some(entry) = named.iter().find(|(k, _)| *k == key) {
            out.push(entry.clone());
        }
    }
    for entry in &named {
        if !out.iter().any(|(k, _)| *k == entry.0) {
            let at = match entry.0 {
                SourceKey::Inst(_) => out.iter().position(|(k, _)| matches!(k, SourceKey::Bus(_))).unwrap_or(out.len()),
                SourceKey::Bus(_) => out.len(),
            };
            out.insert(at, entry.clone());
        }
    }
    out
}

/// The engine routing for a device with `device_channels` outputs: the map's slots (stale ones
/// included — a source that comes back plays on its old pair) and its overflow; the pairs past the
/// map's width stay silent. A map wider than the device is refused.
pub fn session_routing(map: &RoutingMap, device_channels: usize) -> Result<SourceRouting, String> {
    if map.channels > device_channels {
        return Err(format!("the routing map needs {} channels but the output device has {device_channels}", map.channels));
    }
    let pairs = device_channels / 2;
    let mut slots: Vec<Option<SourceKey>> = vec![None; pairs];
    for s in &map.slots {
        let key = parse_source(&s.source).ok_or_else(|| format!("bad source {:?} in the routing map", s.source))?;
        slots[pair_of(s.channel)] = Some(key);
    }
    let routing = SourceRouting { width: pairs * 2, slots, residual: pair_of(map.overflow.channel) };
    routing.validate()?;
    Ok(routing)
}

/// Read a routing map (`None` when there is none yet). One written before the rename to Engines
/// reads as the current format.
pub fn read(path: &Path) -> Result<Option<RoutingMap>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut map: RoutingMap = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            match map.schema.as_str() {
                SCHEMA => {}
                LEGACY_SCHEMA => map.schema = SCHEMA.into(),
                other => return Err(format!("{}: unknown routing map schema {other:?} (want {SCHEMA})", path.display())),
            }
            Ok(Some(map))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write a routing map atomically (pretty JSON + newline).
pub fn write(path: &Path, map: &RoutingMap) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(map).map_err(std::io::Error::other)?;
    text.push('\n');
    crate::piece::write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn src(spec: &[&str]) -> Vec<(SourceKey, String)> {
        spec.iter().map(|s| (parse_source(s).unwrap(), s.split_once(':').unwrap().1.to_string())).collect()
    }

    fn by_channel(m: &RoutingMap) -> Vec<(usize, &str, bool)> {
        m.slots.iter().map(|s| (s.channel, s.source.as_str(), s.stale)).collect()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("engines-routing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("p.routing.json")
    }

    #[test]
    fn sources_fill_pairs_from_1_2_and_the_overflow_takes_the_next() {
        let (m, notes) = assign("p", None, &src(&["inst:sub", "inst:crank", "bus:rv"]), 16, Policy::Grow).unwrap();
        assert_eq!(by_channel(&m), [(1, "inst:sub", false), (3, "inst:crank", false), (5, "bus:rv", false)]);
        assert_eq!(m.overflow, Overflow { channel: 7, shared_with: None, extra: Map::new() });
        assert!(notes.is_empty() && m.unrouted.is_empty());
    }

    #[test]
    fn assignments_stick_when_a_source_is_added() {
        let (m1, _) = assign("p", None, &src(&["inst:c", "bus:rv"]), 16, Policy::Grow).unwrap();
        // "b" sorts between them by id; the old pairs don't move, the overflow stays put, b takes
        // the lowest free pair after the overflow.
        let (m2, _) = assign("p", Some(&m1), &src(&["inst:b", "inst:c", "bus:rv"]), 16, Policy::Grow).unwrap();
        assert_eq!(by_channel(&m2), [(1, "inst:c", false), (3, "bus:rv", false), (7, "inst:b", false)]);
        assert_eq!(m2.overflow.channel, 5);
    }

    #[test]
    fn a_removed_source_keeps_its_pair_until_a_new_one_needs_it() {
        let (m1, _) = assign("p", None, &src(&["inst:a", "inst:b"]), 6, Policy::Grow).unwrap();
        assert_eq!(m1.overflow.channel, 5);
        let (m2, _) = assign("p", Some(&m1), &src(&["inst:b"]), 6, Policy::Grow).unwrap();
        assert_eq!(by_channel(&m2), [(1, "inst:a", true), (3, "inst:b", false)]);
        // A new source on a full device takes the stale pair; the old slot is retired.
        let (m3, notes) = assign("p", Some(&m2), &src(&["inst:b", "inst:c"]), 6, Policy::Grow).unwrap();
        assert_eq!(by_channel(&m3), [(1, "inst:c", false), (3, "inst:b", false)]);
        assert_eq!(m3.retired.iter().map(|s| s.source.as_str()).collect::<Vec<_>>(), ["inst:a"]);
        assert!(notes.iter().any(|n| n.contains("took 1/2")), "{notes:?}");
    }

    #[test]
    fn a_full_device_shares_its_last_pair_with_the_overflow() {
        let (m, notes) = assign("p", None, &src(&["inst:a", "inst:b", "inst:c"]), 4, Policy::Grow).unwrap();
        assert_eq!(by_channel(&m), [(1, "inst:a", false), (3, "inst:b", false)]);
        assert_eq!(m.unrouted, ["inst:c"]);
        assert_eq!(m.overflow, Overflow { channel: 3, shared_with: Some("inst:b".into()), extra: Map::new() });
        assert!(notes.len() == 2, "{notes:?}");
    }

    #[test]
    fn frozen_leaves_the_slots_and_reports_new_sources_as_unrouted() {
        let (m1, _) = assign("p", None, &src(&["inst:a"]), 8, Policy::Grow).unwrap();
        let (m2, _) = assign("p", Some(&m1), &src(&["inst:a", "inst:new"]), 8, Policy::Frozen).unwrap();
        assert_eq!(by_channel(&m2), by_channel(&m1));
        assert_eq!(m2.unrouted, ["inst:new"]);
        assert_eq!(m2.overflow, m1.overflow);
    }

    #[test]
    fn a_narrower_device_reassigns_what_no_longer_fits() {
        let many: Vec<String> = (0..6).map(|i| format!("inst:v{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let (m1, _) = assign("p", None, &src(&refs), 16, Policy::Grow).unwrap();
        let (m2, notes) = assign("p", Some(&m1), &src(&refs), 8, Policy::Grow).unwrap();
        assert_eq!(m2.slots.len(), 4);
        assert!(m2.slots.iter().all(|s| s.channel <= 7));
        assert!(notes.iter().any(|n| n.contains("reassigned")), "{notes:?}");
        assert!(assign("p", None, &src(&refs), 2, Policy::Grow).is_err());
    }

    #[test]
    fn the_session_routing_follows_the_map_on_a_wider_device() {
        let (m, _) = assign("p", None, &src(&["inst:a", "bus:rv"]), 8, Policy::Grow).unwrap();
        let r = session_routing(&m, 16).unwrap();
        assert_eq!(r.width, 16);
        assert_eq!(r.slots[..3], [Some(SourceKey::Inst("a".into())), Some(SourceKey::Bus("rv".into())), None]);
        assert_eq!(r.residual, 2);
        assert!(session_routing(&m, 4).unwrap_err().contains("needs 8 channels"));
    }

    #[test]
    fn the_map_round_trips_through_json() {
        let path = temp("round-trip");
        assert_eq!(read(&path).unwrap(), None);
        let (m, _) = assign("p", None, &src(&["inst:a", "bus:rv"]), 8, Policy::Grow).unwrap();
        write(&path, &m).unwrap();
        assert_eq!(read(&path).unwrap(), Some(m));
        std::fs::write(&path, r#"{"schema":"other","piece":"p","channels":4,"slots":[],"overflow":{"channel":1}}"#).unwrap();
        assert!(read(&path).unwrap_err().contains("schema"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn fields_the_open_map_doesnt_know_survive_a_reassignment() {
        // As Studio's Live set builder writes it: a slot's `midi` and track id, the overflow's track,
        // the set's own fields, and one nobody here has heard of.
        let path = temp("unknown");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let studio = json!({
            "schema": SCHEMA, "piece": "p", "channels": 8,
            "slots": [
                { "channel": 1, "source": "inst:fog", "name": "fog", "track_id": 31, "midi": { "enabled": true, "port": "Engines Out", "channel": 1, "track_id": 40 } },
                { "channel": 3, "source": "bus:rv", "name": "bus_rv", "track_id": 32 }
            ],
            "overflow": { "channel": 5, "track_id": 33 },
            "template": "/T/engines-template.als", "mode": "render", "faders": { "inst:fog": -4.5 },
            "x_studio": { "kept": [1, 2] }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&studio).unwrap()).unwrap();
        let prev = read(&path).unwrap().unwrap();
        assert_eq!(prev.slots[0].extra["midi"]["enabled"], json!(true));
        for policy in [Policy::Frozen, Policy::Grow] {
            // A new source comes along; the player keeps the slots (Frozen), the builder gives it a pair (Grow).
            let (m, _) = assign("p", Some(&prev), &src(&["inst:fog", "inst:new", "bus:rv"]), 8, policy).unwrap();
            write(&path, &m).unwrap();
            let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for key in ["template", "mode", "faders", "x_studio"] {
                assert_eq!(back[key], studio[key], "{policy:?}: top-level {key}");
            }
            assert_eq!(back["slots"][0]["midi"], studio["slots"][0]["midi"], "{policy:?}: the slot's midi");
            assert_eq!((back["slots"][0]["track_id"].clone(), back["slots"][1]["track_id"].clone()), (json!(31), json!(32)), "{policy:?}");
            assert_eq!(back["overflow"]["track_id"], json!(33), "{policy:?}: the overflow stayed on its pair");
            std::fs::write(&path, serde_json::to_string_pretty(&studio).unwrap()).unwrap();
        }
        // A new slot starts with nothing recorded; an overflow that moves leaves its track id behind.
        let (grown, _) = assign("p", Some(&prev), &src(&["inst:fog", "inst:new", "bus:rv"]), 8, Policy::Grow).unwrap();
        assert!(grown.slots.iter().find(|s| s.source == "inst:new").unwrap().extra.is_empty());
        let (narrow, _) = assign("p", Some(&prev), &src(&["inst:fog"]), 4, Policy::Grow).unwrap();
        assert!(narrow.overflow.extra.is_empty(), "{:?}", narrow.overflow);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_map_written_before_the_rename_still_reads() {
        let path = temp("legacy");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"schema":"sme-routing/1","piece":"p","channels":4,"slots":[{"source":"inst:fog","name":"fog","channel":1,"midi":{"enabled":true,"port":"SME Out","channel":1}}],"overflow":{"channel":3}}"#).unwrap();
        let m = read(&path).unwrap().unwrap();
        assert_eq!((m.schema.as_str(), m.slots[0].source.as_str()), (SCHEMA, "inst:fog"), "read as the current format");
        assert_eq!(m.slots[0].extra["midi"]["port"], json!("SME Out"), "kept as it was");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(feature = "stems")]
    #[test]
    fn doc_sources_follow_the_authored_order_with_stem_names() {
        let doc = crate::loop_doc::LoopDoc::from_value(json!({
            "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 4 }, "harmony": { "root": "A", "mode": "major" },
            "instruments": {
                "zed": { "params": { "wave": "sine", "level_db": -10.46 } },
                "alpha": { "params": { "wave": "sine", "level_db": -10.46 } },
                "ext": { "type": "midi", "routing": { "channel": 2 } }
            },
            "buses": { "rv": { "return_gain_db": -10.46, "effects": { "verb": { "type": "reverb", "params": {} } } } }
        }))
        .unwrap();
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let got: Vec<(String, String)> = doc_sources(&doc, &ids(&["zed", "alpha", "ext"]), &ids(&["rv"])).into_iter().map(|(k, n)| (k.to_string(), n)).collect();
        assert_eq!(got, [("inst:zed".into(), "zed".into()), ("inst:alpha".into(), "alpha".into()), ("bus:rv".into(), "bus_rv".into())], "midi skipped, text order kept");
        let unordered: Vec<String> = doc_sources(&doc, &[], &[]).into_iter().map(|(k, _)| k.to_string()).collect();
        assert_eq!(unordered.last().map(String::as_str), Some("bus:rv"), "without an order: instruments, then buses");
    }

    #[test]
    fn sources_parse_back_from_their_display_form() {
        for k in [SourceKey::Inst("pad-2".into()), SourceKey::Bus("rv".into())] {
            assert_eq!(parse_source(&k.to_string()), Some(k));
        }
        assert_eq!(parse_source("inst:"), None);
        assert_eq!(parse_source("fx:rv"), None);
    }
}
