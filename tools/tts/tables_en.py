# /// script
# requires-python = ">=3.10,<3.13"
# dependencies = [
#   "kokoro==0.9.4",
#   "misaki[en]==0.9.4",
#   "soundfile==0.14.0",
#   "numpy",
#   "onnxruntime",
#   "huggingface_hub",
#   "en-core-web-sm @ https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl",
# ]
# ///
"""Writes tts/src/en_gold.tsv and en_silver.tsv from misaki 0.9.4's US
lexicons, as `tts::en` reads them: `word<TAB>phonemes`, or for a heteronym
`word<TAB>TAG=phonemes|TAG=phonemes`, with `~` for a null variant.

    uv run tools/tts/tables_en.py
"""
import importlib.resources, json, os
import misaki.data

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "tts", "src")
for name in ("gold", "silver"):
    lexicon = json.load(importlib.resources.files(misaki.data).joinpath(f"us_{name}.json").open())
    with open(os.path.join(OUT, f"en_{name}.tsv"), "w") as f:
        for word, ps in lexicon.items():
            assert "\t" not in word and "\n" not in word, word
            if isinstance(ps, dict):
                ps = "|".join(f"{tag}={'~' if v is None else v}" for tag, v in ps.items())
            f.write(f"{word}\t{ps}\n")
    print(name, len(lexicon))
