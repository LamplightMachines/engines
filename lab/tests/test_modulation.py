"""Integration tests: a param routed through the matrix to an instrument param must
actually modulate the **output audio** — not merely the effective-state JSON. These render
the engine via the CLI and measure the rendered WAV over time windows, plus an unmodulated
control to prove the variation is *caused by* the modulation.

Motivation: the contract example (v0.7 at the time of the report) routes `seq_swell → instruments.poly.params.level`, but the
swing is tiny (depth 0.20 × a 0.44–0.54 source ≈ 0.02), so it reads as "static". Nothing
asserted that modulation reaches the audio, so a routing/application regression (effective
JSON right, synthesis wrong, or vice-versa) would slip through. These close that gap.
"""
from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import engines_lab as sm

FIXTURE_DIR = Path(__file__).parent / "fixtures"
MOD_FIXTURE = FIXTURE_DIR / "loop_modulation_smoke.json"   # amp_lfo → mono.level
FIXTURE_CONTRACT = FIXTURE_DIR / "loop_contract_v0.9.json"
SR = 16_000
N_WIN = 8


def _windowed_rms(mono: np.ndarray, n: int = N_WIN) -> np.ndarray:
    w = len(mono) // n
    return np.array([float(np.sqrt(np.mean(mono[i * w:(i + 1) * w] ** 2))) for i in range(n)])


def _windowed_centroid(mono: np.ndarray, sr: int, n: int = N_WIN) -> np.ndarray:
    w = len(mono) // n
    return np.array([sm.spectral_centroid(mono[i * w:(i + 1) * w], sr) for i in range(n)])


def _ratio(x: np.ndarray) -> float:
    x = x[x > 1e-9]
    return float(x.max() / x.min()) if x.size and x.min() > 0 else float("inf")


def _render_mono(doc_or_path, tmp_path, name: str) -> np.ndarray:
    if isinstance(doc_or_path, dict):
        doc_or_path = sm.write_doc(doc_or_path, tmp_path / f"{name}.json")
    audio, _ = sm.read_wav(sm.render(doc_or_path, tmp_path / f"{name}.wav", loops=1, sample_rate=SR))
    return audio.mean(axis=0)  # downmix to mono


# ── level modulation → audible amplitude envelope ────────────────────────────

def test_level_modulation_creates_amplitude_envelope(tmp_path):
    mono = _render_mono(MOD_FIXTURE, tmp_path, "lvlmod")
    rms = _windowed_rms(mono)
    assert _ratio(rms) > 2.0, f"amp_lfo → mono.level should swing loudness across the loop: {rms}"


def test_no_modulation_audio_is_flat(tmp_path):
    # Same doc with the level-modulation matrix entry removed → level is constant → flat RMS.
    # Proves the envelope above is caused by the modulation, not by note/filter transients.
    doc = json.loads(MOD_FIXTURE.read_text())
    del doc["matrix"]["m_amp"]
    mono = _render_mono(doc, tmp_path, "nomod")
    rms = _windowed_rms(mono)
    assert _ratio(rms) < 1.3, f"with no level modulation the loudness should be ~flat: {rms}"


# ── cutoff modulation → audible brightness variation ─────────────────────────

def test_cutoff_modulation_varies_brightness(tmp_path):
    # Re-point the LFO at the filter cutoff (saw has harmonics to sweep). Base 800 Hz ± 3 octaves
    # sweeps ~100 Hz–6.4 kHz; the spectral centroid follows it (genuine timbre change).
    doc = json.loads(MOD_FIXTURE.read_text())
    doc["instruments"]["mono"]["params"]["cutoff_hz"] = 800
    doc["matrix"]["m_amp"]["dest"] = "instruments.mono.params.cutoff_hz"
    doc["matrix"]["m_amp"].pop("depth_db")
    doc["matrix"]["m_amp"]["depth_oct"] = 2.99
    # Skip window 0 — it catches the note's onset transient (broadband attack), not the
    # modulation (CE-R9.3: exclude attack transients from attributed brightness).
    mono = _render_mono(doc, tmp_path, "cutmod")
    cen = _windowed_centroid(mono, SR)[1:]
    assert _ratio(cen) > 1.5, f"cutoff modulation should swing the spectral centroid: {cen}"

    # control: no cutoff modulation → ~flat brightness
    del doc["matrix"]["m_amp"]
    mono0 = _render_mono(doc, tmp_path, "cutmod0")
    cen0 = _windowed_centroid(mono0, SR)[1:]
    assert _ratio(cen0) < 1.3, f"with no cutoff modulation, brightness should be ~flat: {cen0}"
