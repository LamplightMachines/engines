"""Lab interface to the Rust engine over the **file/CLI boundary** (no pybind11).

An early C++/pybind11 bridge is gone (rewrite → Rust core + Swift shell, ADR
0003). The lab now drives the engine the same way any consumer does: it shells out to the
`engines-render` / `engines-song` binaries and reads their WAV / JSONL outputs. This keeps the lab
on the supported boundary and makes the Python tests a language-agnostic acceptance spec.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parents[2]
RELEASE = REPO / "target" / "release"


def _bin(name: str) -> Path:
    p = RELEASE / name
    if not p.exists():
        raise FileNotFoundError(f"{p} not built — run `cargo build --release --bin {name}`")
    return p


def render(doc_path, out_wav, *, loops: int = 1, sample_rate: int = 48_000) -> Path:
    """Run `engines-render <doc> <out.wav> <loops> <sr>` → the written WAV path."""
    subprocess.run(
        [str(_bin("engines-render")), str(doc_path), str(out_wav), str(loops), str(sample_rate)],
        cwd=REPO, check=True, capture_output=True, text=True,
    )
    return Path(out_wav)


def song_render(doc_path, out_wav, *, max_loops: int | None = None, sample_rate: int = 48_000,
                loop_log=None, extra: list[str] | None = None) -> Path:
    """Run `engines-song --doc <doc> --out <out.wav> --whole …` (the live Session path: drift / Orrery /
    songs) → the written WAV path. `--whole` keeps a song in that one file (without it, ADR 0043
    writes a song one file per block into an `out/` directory). A song-less doc needs `max_loops`.
    `loop_log` also writes one JSON line per rendered loop; `extra` appends raw flags (e.g.
    `["--orrery"]`)."""
    cmd = [str(_bin("engines-song")), "--doc", str(doc_path), "--out", str(out_wav), "--whole",
           "--sample-rate", str(sample_rate)]
    if max_loops is not None:
        cmd += ["--max-loops", str(max_loops)]
    if loop_log is not None:
        cmd += ["--loop-log", str(loop_log)]
    cmd += extra or []
    subprocess.run(cmd, cwd=REPO, check=True, capture_output=True, text=True)
    return Path(out_wav)


def read_loop_log(path) -> list[dict]:
    """Parse an `engines-song --loop-log` JSONL file into a list of per-loop record dicts."""
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]


def read_wav(path) -> tuple[np.ndarray, int]:
    """Read a 32-bit-float WAV (what `engines_core::wav` writes) → (`[channels, frames]`, sr)."""
    raw = Path(path).read_bytes()
    if raw[:4] != b"RIFF" or raw[8:12] != b"WAVE":
        raise ValueError(f"{path}: not a RIFF/WAVE file")
    channels, sample_rate, data = 2, 48_000, None
    i = 12
    while i + 8 <= len(raw):
        cid = raw[i:i + 4]
        size = int.from_bytes(raw[i + 4:i + 8], "little")
        body = raw[i + 8:i + 8 + size]
        if cid == b"fmt ":
            channels = int.from_bytes(body[2:4], "little")
            sample_rate = int.from_bytes(body[4:8], "little")
        elif cid == b"data":
            data = np.frombuffer(body, dtype="<f4")
        i += 8 + size + (size & 1)  # chunks are word-aligned
    if data is None:
        raise ValueError(f"{path}: no data chunk")
    return data.reshape(-1, channels).T.copy(), sample_rate


def minimal_doc(**overrides) -> dict:
    """A minimal v0.9 loop doc (native units, ADR 0054): one mono instrument struck at step 0 (id-keyed modulators
    /matrix per the id-keyed schema). Override top-level keys via kwargs; mutate nested dicts in the caller."""
    doc = {
        "schema_version": "0.9",
        "loop_id": 1,
        "transport": {"bpm": 120, "master_steps": 4, "step_unit": "beat"},
        "harmony": {"root": "A", "mode": "major"},
        "global": {"master_gain_db": 0.0, "stereo_width": 0.5},
        "instruments": {
            "mono": {
                "enabled": True, "pitched": True,
                "note_engine": {"polyphony": 1, "glide_ms": 0},
                "params": {"wave": "sine", "level_db": -6.02, "cutoff_hz": 800, "drive_db": 0.0},
            }
        },
        "modulators": {
            "seq": {
                "type": "step_seq", "output": "note", "sync": "loop", "length": 4,
                "steps": [{"step": 0, "voicing": {"v1": {"offset": 0}}}],
            }
        },
        "matrix": {
            "m1": {"source": "seq", "dest": "instruments.mono.notes",
                   "kind": "note", "voices": 1, "priority": 1},
        },
    }
    doc.update(overrides)
    return doc


def write_doc(doc: dict, path) -> Path:
    Path(path).write_text(json.dumps(doc))
    return Path(path)


def rms(a: np.ndarray) -> float:
    return float(np.sqrt(np.mean(a.astype(np.float64) ** 2)))


def spectral_centroid(mono: np.ndarray, sample_rate: int) -> float:
    mag = np.abs(np.fft.rfft(mono.astype(np.float64)))
    freqs = np.fft.rfftfreq(len(mono), 1.0 / sample_rate)
    total = mag.sum()
    return float(np.dot(freqs, mag) / total) if total > 0 else 0.0
