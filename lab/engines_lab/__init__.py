"""Engines — composing instrument for long-form generative tracks, Python lab.

The lab drives the **Rust** engine over the file/CLI boundary (no pybind11; an early C++
bridge was replaced by the Rust core + Swift shell, ADR 0003). See `cli` for the
`engines-render` / `engines-song` wrappers and the WAV / loop-log readers.
"""

from .cli import (
    minimal_doc,
    read_loop_log,
    read_wav,
    render,
    rms,
    song_render,
    spectral_centroid,
    write_doc,
)

__all__ = [
    "render",
    "song_render",
    "read_loop_log",
    "read_wav",
    "minimal_doc",
    "write_doc",
    "rms",
    "spectral_centroid",
]
