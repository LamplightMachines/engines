# <Title>

`<slug>` · status: **sketch** (sketch → live → song → rendered → published)

## Intent
<The idea in the user's words, then one line each: use, target length, mood.>

- Root / mode: <root> <mode> · <bpm> bpm · <master_steps>-beat loop = <loop_s> s
- Starting point: <from scratch | house style: <name> | piece: <slug>>

## Levers
| lever | choice | how the doc does it |
|---|---|---|
| density | <sparse…busy> | <e.g. euclid 3/16 pulse, prob 0.6 ghosts, 3-voice pad> |
| brightness | <dark…airy> | <e.g. cutoff_hz 400, saw_soft halo +12, shimmer 0.3> |
| structure | <predictable…messy> | <e.g. loop-synced drones; free 23-step forks; LFOs 0.045/0.031/0.021 Hz → realigns ≈ N loops> |
| mood | <mode, Orrery off/tamed/roaming> | <e.g. dorian; Orrery tamed: phrase_loops 4, tension 0.12> |

## Voice sheet
| id | role | synth | register | driven by | routes / sends | Orrery |
|---|---|---|---|---|---|---|
| | | | | | | |

## Song plan
_Not yet — add one with `/engines-song`._

## Engine notes
_None yet._ (`<date> — <what's missing> (…)`; offer to file each as an Engine request.)

## Inspiration
_None — delete this section if the piece didn't start from, or borrow from, a snippet or a MIDI file (Engines Studio's `engines-listen` and `engines-midi` skills fill it in)._

- Source: <Artist – Title>, <m:ss–m:ss> (`inspiration/<snippet>/`)
- Taken: <sound | feel | both> · literal notes: <no (default) | opt-in: why>
- Verdict: <recognizably close | close but… | not close>
- Gaps: <what the engine couldn't match>
- ECRs: <ids from `inspiration/requests/`, and issue links once filed>

For a MIDI file (`midi/<name>/`, ADR 0053; the `engines-midi` skill, with Engines Studio):
- MIDI: <file.mid> (fnv1a <hash>) — <the analysis summary, e.g. "D mixolydian, v–IV–I, two motifs">
- Key: <the chosen one> (alternatives: <same-notes keys, e.g. A dorian>)
- Taken: <chords | bass | top line | other line> on <halo | bass | lead | pulse> · as written: <no (default) | opt-in: why>
- Compare: top line <score> · other line <score> (each under 0.3 unless opted in) · chords <score> (reported)

## Renders
_None yet._ (`engines-song --piece <slug>` writes `renders/<date>_r<rev>/` + a `.json` sidecar;
`engines-clean <slug>` lists old ones.)

## Notes

## Changelog
- <YYYY-MM-DD> — r1 — created from the kickoff interview.
