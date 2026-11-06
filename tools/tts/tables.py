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
import json, os
from huggingface_hub import hf_hub_download
# The ONNX export that fetch.sh pins carries no vocab, so this pins the source model.
KOKORO_REVISION = "f3ff3571791e39611d31c381e3a41a3af07b4987"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "tts", "src")
with open(f"{OUT}/tables.rs", "w") as f:
    f.write("// Generated from Kokoro-82M's `config.json` vocab by tools/tts/tables.py.\n// Do not edit by hand.\n\n")
    f.write("pub const VOCAB: &[(char, i64)] = &[\n")
    vocab = json.load(open(hf_hub_download("hexgrad/Kokoro-82M", "config.json", revision=KOKORO_REVISION)))["vocab"]
    for k, v in vocab.items():
        assert len(k) == 1
        char = {"'": "'\\''", "\\": "'\\\\'"}.get(k, f"'{k}'")
        f.write(f"    ({char}, {v}),\n")
    f.write("];\n")
print(len(vocab))
