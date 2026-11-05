#!/usr/bin/env bash
# Writes target/licenses/LICENSES.txt: LICENSE, NOTICE and the full licence text
# of everything the APK carries. android/build.sh packs it into the APK, :8080
# serves it at /licenses, and a release attaches it beside the APK.
# The payload's Python licences are committed in tools/licenses/python, because
# only a component rebuild (private access) unpacks the wheels they come from.
# componentize-py's runtime crates are not in our Cargo.lock: that file is
# `cargo about generate` over runtime/Cargo.toml in componentize-py v0.25.0.
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=target/licenses
WHEELS=airoa-sdk/build/.work/wheels
PYTHON=tools/licenses/python
mkdir -p "$OUT"

# After a rebuild, the committed texts must match the wheels it installed.
if [ -d "$WHEELS" ]; then
  for dist in "$WHEELS"/*.dist-info; do
    name=$(basename "$dist" .dist-info)
    licence=$(find "$dist" -type f \( -iname 'LICENSE*' -o -iname 'COPYING*' \) | head -1)
    [ -n "$licence" ] || { echo "no licence file in $dist" >&2; exit 1; }
    cmp -s "$licence" "$PYTHON/$name.txt" ||
      { echo "$PYTHON/$name.txt is missing or differs: cp $licence $PYTHON/$name.txt" >&2; exit 1; }
  done
  for txt in "$PYTHON"/*.txt; do
    [ -d "$WHEELS/$(basename "$txt" .txt).dist-info" ] ||
      { echo "$txt is not in the payload any more: delete it" >&2; exit 1; }
  done
fi

# The app package alone, because yubi-export is not in the APK.
cargo about generate --locked --fail -c tools/licenses/about.toml \
  -o "$OUT/crates.txt" tools/licenses/about.hbs

section() {
  printf '\n\n================================================================================\n%s\n================================================================================\n\n' "$1"
  cat "$2"
}

{
  cat LICENSE
  section NOTICE NOTICE
  for txt in "$PYTHON"/*.txt; do
    section "$(basename "$txt" .txt) (Python, in airoa-core.cwasm)" "$txt"
  done
  section "CPython 3.14.0 (in airoa-core.cwasm)" tools/licenses/cpython-3.14.0.txt
  section "CPython 3.14.0: expat, HACL*, libmpdec, zlib, zstd bindings (in airoa-core.cwasm)" \
    tools/licenses/cpython-3.14.0-bundled.txt
  section "musl, as wasi-libc carries it (in airoa-core.cwasm)" tools/licenses/wasi-libc-musl-COPYRIGHT.txt
  section "cloudlibc, as wasi-libc carries it (in airoa-core.cwasm)" tools/licenses/wasi-libc-cloudlibc-LICENSE.txt
  section "componentize-py 0.25.0 runtime crates (in airoa-core.cwasm)" \
    tools/licenses/componentize-py-0.25.0-runtime-crates.txt
  section "jsoncpp, as the OpenXR loader 1.1.62 carries it" tools/licenses/jsoncpp-openxr-1.1.62.txt
  section "ONNX Runtime 1.31.1" tools/licenses/onnxruntime-1.31.1-LICENSE.txt
  section "ONNX Runtime 1.31.1: third-party notices" tools/licenses/onnxruntime-1.31.1-ThirdPartyNotices.txt
  section "Noto fonts" assets/fonts/LICENSE-OFL.txt
  section "webxr-input-profiles assets (controller meshes)" tools/licenses/webxr-input-profiles-assets.txt
  section "ros2/geometry2 (tf2_msgs/TFMessage.msg)" tools/licenses/ros2-geometry2-LICENSE.txt
  section "foxglove-sdk (CompressedVideo.msg)" tools/licenses/foxglove-sdk.txt
  section "spaCy (English tokenizer rules in tts)" tools/licenses/spacy.txt
  section "Rust standard library 1.99.1 (in libyubi_go.so)" tools/licenses/rust-std-1.99.1.txt
  section "Rust crates" "$OUT/crates.txt"
} >"$OUT/LICENSES.txt"
echo "licences in $OUT/LICENSES.txt ($(du -h "$OUT/LICENSES.txt" | cut -f1))"
