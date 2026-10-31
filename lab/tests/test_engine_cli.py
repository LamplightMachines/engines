"""Engine acceptance tests over the CLI/file boundary (ported from the M2a/M2b suite,
which drove the now-deleted C++ pybind extension). These drive the Rust `engines-render` /
`engines-song` binaries and analyze their WAV / JSONL outputs — a language-agnostic spec that
also catches CLI/contract regressions. Fine-grained DSP behaviors additionally have Rust
unit tests in `crates/engines_core`; these assert them across the shipping boundary.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

import numpy as np
import engines_lab as sm

FIXTURE = Path(__file__).parent / "fixtures" / "loop_contract_v0.9.json"
SR_FAST = 16_000   # spectral/time behaviors hold at low SR; keeps renders quick
SR_HI = 48_000


# ── structural backbone (was M2a) ────────────────────────────────────────────

def test_fixture_frame_count(tmp_path):
    # 65 bpm × 16 beats → round(16 × 60/65 × 48000) frames
    audio, sr = sm.read_wav(sm.render(FIXTURE, tmp_path / "o.wav", loops=1, sample_rate=SR_HI))
    assert sr == SR_HI
    assert audio.shape[0] == 2
    assert audio.shape[1] == round(16 * (60 / 65) * SR_HI)


def test_audio_stereo_finite_nonzero_reasonable(tmp_path):
    audio, _ = sm.read_wav(sm.render(FIXTURE, tmp_path / "o.wav", sample_rate=SR_HI))
    assert audio.shape[0] == 2
    assert np.all(np.isfinite(audio)), "audio has NaN/Inf"
    assert not np.all(audio == 0.0), "all audio is zero"
    assert float(np.max(np.abs(audio))) < 100.0, "peak amplitude looks wrong"


def test_determinism_bit_identical(tmp_path):
    a = sm.render(FIXTURE, tmp_path / "a.wav", sample_rate=SR_HI)
    b = sm.render(FIXTURE, tmp_path / "b.wav", sample_rate=SR_HI)
    assert Path(a).read_bytes() == Path(b).read_bytes(), "render is not bit-identical for same inputs"


def test_second_loop_differs_from_first(tmp_path):
    audio, _ = sm.read_wav(sm.render(FIXTURE, tmp_path / "o.wav", loops=2, sample_rate=SR_FAST))
    half = audio.shape[1] // 2
    assert not np.array_equal(audio[:, :half], audio[:, half:]), "free modulator phase should advance across loops"


def test_the_doc_writer_writes_minus_infinity_and_python_reads_it(tmp_path):
    # ADR 0054 P3: what the engine writes about a doc (a render's sidecar here, its control holding a
    # `-inf` floor) says `-Infinity`, which Python's `json` reads — not `null`.
    piece = tmp_path / "pieces" / "demo"
    piece.mkdir(parents=True)
    sm.write_doc(sm.minimal_doc(), piece / "demo.json5")
    subprocess.run([str(sm.cli._bin("engines-song")), "--piece", "demo", "--max-loops", "1", "--sample-rate", "16000",
                    "--floor", "instruments.mono.params.level_db=-inf"], cwd=tmp_path, check=True, capture_output=True, text=True)
    (sidecar,) = (piece / "renders").glob("*.json")
    side = json.loads(sidecar.read_text())
    assert side["control"]["guardrails"]["floors"]["instruments.mono.params.level_db"] == float("-inf")


# ── DSP quality (was M2b) ────────────────────────────────────────────────────

def test_waveforms_produce_distinct_audio(tmp_path):
    audios = {}
    for wave in ("sine", "triangle", "saw_soft", "sine_bell"):
        d = sm.minimal_doc()
        d["instruments"]["mono"]["params"]["wave"] = wave
        d["instruments"]["mono"]["params"]["decay_s"] = 0.0
        p = sm.write_doc(d, tmp_path / f"{wave}.json")
        audios[wave] = sm.read_wav(sm.render(p, tmp_path / f"{wave}.wav", sample_rate=SR_FAST))[0].flatten()
    waves = list(audios)
    for i in range(len(waves)):
        for j in range(i + 1, len(waves)):
            assert not np.allclose(audios[waves[i]], audios[waves[j]]), f"{waves[i]} and {waves[j]} identical"


def test_drive_increases_rms(tmp_path):
    def rms_for(drive_db: float) -> float:
        d = sm.minimal_doc()
        d["instruments"]["mono"]["params"]["drive_db"] = drive_db
        p = sm.write_doc(d, tmp_path / f"drive{drive_db}.json")
        return sm.rms(sm.read_wav(sm.render(p, tmp_path / f"drive{drive_db}.wav", sample_rate=SR_FAST))[0])
    assert rms_for(19.0) > rms_for(0.0), "drive (soft-clip + gain) should raise RMS"


def test_cutoff_shifts_spectral_centroid(tmp_path):
    centroids = []
    for cutoff in (101, 800, 6340):  # Hz
        d = sm.minimal_doc()
        d["instruments"]["mono"]["params"]["wave"] = "saw_soft"  # rich harmonics to filter
        d["instruments"]["mono"]["params"]["cutoff_hz"] = cutoff
        p = sm.write_doc(d, tmp_path / f"cut{cutoff}.json")
        audio, sr = sm.read_wav(sm.render(p, tmp_path / f"cut{cutoff}.wav", sample_rate=SR_HI))
        centroids.append(sm.spectral_centroid(audio[0], sr))
    assert centroids[0] < centroids[1] < centroids[2], f"centroid not monotonic with cutoff: {centroids}"


def test_stereo_width_0_is_mono(tmp_path):
    d = sm.minimal_doc()
    d["global"]["stereo_width"] = 0.0
    p = sm.write_doc(d, tmp_path / "w0.json")
    audio, _ = sm.read_wav(sm.render(p, tmp_path / "w0.wav", sample_rate=SR_FAST))
    np.testing.assert_allclose(audio[0], audio[1], atol=1e-6, err_msg="stereo_width=0 must produce L==R")


def test_audio_finite_after_full_dsp_chain(tmp_path):
    d = sm.minimal_doc()
    d["buses"] = {"rv": {"return_gain_db": -10.46, "effects": {"verb": {"type": "reverb", "params": {"room": 0.5, "damping": 0.5}}}}}
    d["instruments"]["mono"]["sends_db"] = {"rv": 0}
    d["instruments"]["mono"]["params"].update({"drive_db": 14.8, "cutoff_hz": 201})
    p = sm.write_doc(d, tmp_path / "dsp.json")
    audio, _ = sm.read_wav(sm.render(p, tmp_path / "dsp.wav", sample_rate=SR_FAST))
    assert np.all(np.isfinite(audio)), "audio must be finite after reverb+drive+filter"


# ── the live Session path + its per-loop log (engines-song --loop-log) ─────────────

def test_loop_log_records_every_loop_and_tiles_the_audio(tmp_path):
    log = tmp_path / "loops.jsonl"
    wav = sm.song_render(FIXTURE, tmp_path / "o.wav", max_loops=3, sample_rate=SR_FAST, loop_log=log)
    audio, sr = sm.read_wav(wav)
    recs = sm.read_loop_log(log)
    assert len(recs) == 3, "one record per rendered loop"
    assert [r["loop_id"] - recs[0]["loop_id"] for r in recs] == [0, 1, 2]
    starts = [r["start_frame"] for r in recs]
    assert starts[0] == 0 and all(b - a == r["num_frames"] for a, b, r in zip(starts, starts[1:], recs))
    assert sum(r["num_frames"] for r in recs) == audio.shape[1], "records tile the rendered audio"
    assert abs(max(r["peak"] for r in recs) - float(np.max(np.abs(audio)))) < 1e-6, "peaks match the WAV"


def test_song_render_of_a_song_doc_is_one_wav(tmp_path):
    # A song renders one file per block unless `--whole` (ADR 0043); the helper keeps its one-WAV contract.
    song = Path(__file__).parent / "fixtures" / "song_glass_garden.json5"
    wav = sm.song_render(song, tmp_path / "o.wav", max_loops=2, sample_rate=SR_FAST)
    assert wav.is_file() and not (tmp_path / "o").exists()
    audio, sr = sm.read_wav(wav)
    assert sr == SR_FAST and audio.shape[1] == 2 * round(12 * 60 / 54 * SR_FAST)  # 2 loops of 12 beats @ 54 bpm


def test_loop_log_drift_moves_a_canonical_path(tmp_path):
    log = tmp_path / "loops.jsonl"
    sm.song_render(FIXTURE, tmp_path / "o.wav", max_loops=2, sample_rate=SR_FAST, loop_log=log,
                   extra=["--max-change", "0.05"])
    recs = sm.read_loop_log(log)
    moved = [r for r in recs if r.get("path")]
    assert moved, "drift moves a param"
    for r in moved:
        assert "." in r["path"], "a canonical JSONPath"
        assert r["optimize"] == "drift"
