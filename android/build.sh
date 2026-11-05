#!/usr/bin/env bash
# Build yubi-go.apk with direct SDK tools — no Gradle.
set -euo pipefail
cd "$(dirname "$0")/.."

# No fallback: the SDK lives somewhere different on every machine, and guessing
# only turns "ANDROID_HOME is unset" into "aapt2: No such file" ten lines later.
SDK="${ANDROID_HOME:?set ANDROID_HOME to your Android SDK}"
BT="$SDK/build-tools/33.0.0"
PLATFORM="$SDK/platforms/android-33/android.jar"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$SDK/ndk/27.2.12479018}"
for needed in "$BT/aapt2" "$PLATFORM" "$ANDROID_NDK_HOME"; do
  [ -e "$needed" ] || { echo "missing $needed — install it with sdkmanager" >&2; exit 1; }
done

OUT=target/apk
LIB=target/aarch64-linux-android/release/libyubi_go.so
KEYSTORE="$HOME/.android/debug.keystore"

cargo ndk -t arm64-v8a -P 29 build --release --locked

# The device refuses to start without airoa-core.cwasm, and refuses one from a
# different wasmtime. Both stages (Component, then AOT) skip themselves when
# their inputs have not moved, so this is cheap on an incremental build.
CWASM=airoa-sdk/build/airoa-core-aarch64.cwasm
cargo build-cwasm aarch64-linux-android

rm -rf "$OUT" && mkdir -p "$OUT/lib/arm64-v8a" "$OUT/assets/airoa"
cp "$LIB" "$OUT/lib/arm64-v8a/"
cp android/jniLibs/arm64-v8a/*.so "$OUT/lib/arm64-v8a/"
# The payload (only the cwasm reaches a device) travels in the APK, and the app
# copies it into files/airoa at launch whenever its stamp differs
# (`misc::core::unpack`), so no headset needs it pushed by hand.
cp "$CWASM" "$OUT/assets/airoa/airoa-core.cwasm"
cp "$CWASM.src" "$OUT/assets/airoa/airoa-core.cwasm.src"
# Spoken task names (`misc::speech`): the engine travels the same way, into
# files/tts, and ONNX Runtime sits beside libyubi_go.so to be loaded by name.
tools/tts/fetch.sh
cp -R target/tts/speech "$OUT/assets/tts"
cp target/tts/libonnxruntime.so "$OUT/lib/arm64-v8a/"
# Every licence text the APK owes its recipients (`tools/licenses/bundle.sh`).
tools/licenses/bundle.sh
cp target/licenses/LICENSES.txt "$OUT/assets/LICENSES.txt"

"$BT/aapt2" compile --dir android/res -o "$OUT/res.zip"
"$BT/aapt2" link -o "$OUT/unaligned.apk" \
  --manifest android/AndroidManifest.xml \
  -I "$PLATFORM" "$OUT/res.zip" -A "$OUT/assets" \
  --min-sdk-version 24 --target-sdk-version 29

(cd "$OUT" && zip -q -r unaligned.apk lib)

"$BT/zipalign" -f 4 "$OUT/unaligned.apk" "$OUT/yubi-go.apk"

# Official releases are signed with AIRoA's release key, so they update each
# other whoever builds them. Every other build uses this machine's debug key.
if [ -n "${YUBI_GO_KEYSTORE:-}" ]; then
  "$BT/apksigner" sign --ks "$YUBI_GO_KEYSTORE" --ks-pass env:YUBI_GO_KEYSTORE_PASS \
    "$OUT/yubi-go.apk"
else
  if [ ! -f "$KEYSTORE" ]; then
    mkdir -p "$(dirname "$KEYSTORE")"
    keytool -genkeypair -keystore "$KEYSTORE" -storepass android -keypass android \
      -alias androiddebugkey -dname CN=Debug -keyalg RSA -validity 10000
  fi
  "$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android "$OUT/yubi-go.apk"
fi

echo "built $OUT/yubi-go.apk"
echo "install: adb install -r $OUT/yubi-go.apk   (the airoa_core payload is inside it)"
echo "watch:   adb logcat -s yubigo"
