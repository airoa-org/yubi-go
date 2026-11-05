#!/usr/bin/env bash
# Lays out what the headset speaks with in target/tts/speech/ (packed into the
# APK by android/build.sh, unpacked into files/tts by the app) and puts
# ONNX Runtime's Android library at target/tts/libonnxruntime.so. Downloads
# are pinned and checksummed, cached in target/tts/cache, and done once.
set -euo pipefail
cd "$(dirname "$0")/../.."
CACHE=target/tts/cache
OUT=target/tts/speech
mkdir -p "$CACHE"

HF=https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/1939ad2a8e416c0acfeecc08a694d14ef25f2231
# The 8-bit model (88 MB): fp32 (310 MB) ran about twice as slow on the
# Quest 3S, and the two are barely distinguishable by ear.
# name  url  sha256
PINS=(
  "model.onnx $HF/onnx/model_quantized.onnx fbae9257e1e05ffc727e951ef9b9c98418e6d79f1c9b6b13bd59f5c9028a1478"
  "af_heart.bin $HF/voices/af_heart.bin d583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b"
  "ort.aar https://repo1.maven.org/maven2/com/microsoft/onnxruntime/onnxruntime-android/1.31.1/onnxruntime-android-1.31.1.aar d4d50660300ed2c4f38ffdfd5ab5bfa3a22f995f282dda24f42ea3ea55467b49"
)

for pin in "${PINS[@]}"; do
  read -r name url sum <<<"$pin"
  if ! echo "$sum  $CACHE/$name" | shasum -a 256 -c --status 2>/dev/null; then
    echo "fetching $name"
    curl -fsSL -o "$CACHE/$name.part" "$url"
    echo "$sum  $CACHE/$name.part" | shasum -a 256 -c --status \
      || { echo "$name: checksum mismatch for $url" >&2; exit 1; }
    mv "$CACHE/$name.part" "$CACHE/$name"
  fi
done

# Everything the output depends on; the app re-unpacks when it changes.
STAMP=$(shasum -a 256 tools/tts/fetch.sh | cut -c1-16)
if [ "$(cat "$OUT/STAMP" 2>/dev/null)" != "$STAMP" ]; then
  rm -rf "$OUT"
  mkdir -p "$OUT"
  cp "$CACHE/model.onnx" "$CACHE/af_heart.bin" "$OUT/"
  (cd target/tts && unzip -o -q -j cache/ort.aar 'jni/arm64-v8a/libonnxruntime.so')
  (cd "$OUT" && find . -type f ! -name FILES | sed 's#^\./##' | sort >FILES)
  echo "$STAMP" >"$OUT/STAMP"
  echo "speech files in $OUT ($(du -sh "$OUT" | cut -f1))"
fi
