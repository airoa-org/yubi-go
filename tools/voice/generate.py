# /// script
# requires-python = ">=3.10,<3.13"
# dependencies = [
#   "kokoro==0.9.4",
#   "misaki[en]==0.9.4",
#   "soundfile==0.14.0",
#   "numpy",
#   "en-core-web-sm @ https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl",
# ]
# ///
"""Speaks assets/voice/phrases.tsv into assets/voice/<lang>/<id>.wav.

    uv run tools/voice/generate.py                   # every clip
    uv run tools/voice/generate.py en                # one language
    uv run tools/voice/generate.py ready not_ready   # only these ids, every language

Kokoro's output differs slightly between runs, so regenerate only what changed.

The app embeds the clips (src/view/voice.rs), so rerun this and rebuild after
editing a phrase.
"""
import csv
import os
import sys
from pathlib import Path

# The espeak-ng data bundled with espeakng_loader has a broken path on macOS;
# Homebrew's espeak-ng works.
BREW = "/opt/homebrew/opt/espeak-ng"
if os.path.isdir(BREW):
    import espeakng_loader

    espeakng_loader.get_library_path = lambda: f"{BREW}/lib/libespeak-ng.dylib"
    espeakng_loader.get_data_path = lambda: f"{BREW}/share/espeak-ng-data"

# en_core_web_sm's tokenizer crashes on load (OverflowError in
# _flush_specials) under spacy 3.8; misaki does not need it.
import spacy

_load = spacy.load
spacy.load = lambda name, **kw: _load(name, exclude=["tokenizer"], **kw)

import numpy as np
import soundfile as sf
from kokoro import KModel, KPipeline

REPO = "hexgrad/Kokoro-82M"
RATE = 24_000
# lang column -> (Kokoro lang code, voice), chosen by ear on 2026-10-25.
VOICES = {"en": ("a", "af_heart")}
# Every clip peaks here, so the app's one gain sets them all.
PEAK = 0.9
# Kokoro pads ~0.2 s of near-silence at each end; trimmed to this.
PAD_S = 0.03
FLOOR = 0.01

ROOT = Path(__file__).resolve().parents[2] / "assets" / "voice"


def trimmed(audio):
    loud = np.flatnonzero(np.abs(audio) > FLOOR * np.abs(audio).max())
    pad = int(PAD_S * RATE)
    audio = audio[max(loud[0] - pad, 0) : loud[-1] + pad]
    fade = min(pad, len(audio) // 2)
    ramp = np.linspace(0.0, 1.0, fade)
    audio[:fade] *= ramp
    audio[-fade:] *= ramp[::-1]
    return audio


def main():
    with open(ROOT / "phrases.tsv", newline="") as f:
        rows = list(csv.DictReader(f, delimiter="\t"))
    langs = [a for a in sys.argv[1:] if a in VOICES] or [k for k in rows[0] if k != "id"]
    ids = [a for a in sys.argv[1:] if a not in VOICES]
    rows = [row for row in rows if not ids or row["id"] in ids]
    model = KModel(repo_id=REPO).eval()
    for lang in langs:
        code, voice = VOICES[lang]
        pipe = KPipeline(lang_code=code, repo_id=REPO, model=model)
        out = ROOT / lang
        out.mkdir(exist_ok=True)
        for row in rows:
            chunks = [r.audio.numpy() for r in pipe(row[lang], voice=voice) if r.audio is not None]
            audio = trimmed(np.concatenate(chunks))
            audio *= PEAK / np.abs(audio).max()
            sf.write(out / f"{row['id']}.wav", audio, RATE, subtype="PCM_16")
            print(f"{lang} {row['id']:32} {len(audio) / RATE:4.1f} s  {row[lang]}")


main()
