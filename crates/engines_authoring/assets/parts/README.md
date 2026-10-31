# Parts — multi-section fragments merged into a loop doc (ADR 0039)

A part is one musical idea that spans several sections of a doc: an instrument, the modulator
that plays it, the route between them, its reverb send, sometimes a sequence pool or a bus. Adding
one (`engines-part add`, the editor's **Add part** source action, the extension's `Engines: Add a Part…`;
`engines-part add --file` for a part file kept elsewhere, ADR 0050) merges every section at once: comments kept, ids made unique, the doc's own layout followed.

Each `<name>.json5` is a JSON5 object:

- `part` — `{ summary, target?, key? }`. `summary` is the one line menus show. `target` names a param
  (`"cutoff_hz"`) or `"any"` when the part acts on an **existing** instrument (`$target`). `key`
  (`"D mixolydian"`) marks a part whose offsets are raw semitones from that root instead of `$deg`
  — what `engines-midi parts` writes from a MIDI file (ADR 0053): it joins only a doc on the same root
  with the same notes, and is refused anywhere else (another root would transpose it).
- `instruments`, `modulators`, `sequence_pools`, `matrix`, `buses` — members added to those
  sections (a missing section is created).
- `safe_bounds` — merged only when the doc already has a `safe_bounds` section: a doc that freezes
  its levels (the house style, ADR 0032) gets the part's level frozen too; one that lets levels
  drift lets the part's drift. A band the doc already has for that path stays as authored.
- `patch` — `{ "<JSONPath>": value }` set in the doc's existing objects (a send from `$target` to
  a new bus), creating missing objects on the way.

Placeholders, resolved before merging:

| placeholder | becomes |
|---|---|
| `$id` | the part's id: its file name, or `--id`; `_2`, `_3`… when that's taken |
| `$bus` | the doc's send bus (the first that isn't an `external_return`, or `--bus`). A `$bus` send is dropped when the doc has none |
| `$target` | an existing instrument: the one under the cursor, `--target`, or the first that has `part.target` |
| `"$deg(N)"`, `"$deg(N, oct)"` | the semitones of scale degree N (1 = the root) in the doc's mode, `oct` octaves up/down |

Write parts quiet (they join a gain-staged doc, ADR 0021) and key-agnostic: `$deg` for pitches,
`fit: "chord"` for phrase cells (a generated part pinned to a key says so with `part.key`). The test merges every part into every house piece and checks that
the result parses, checks clean, keeps every comment and has unique ids, and renders without clipping.
