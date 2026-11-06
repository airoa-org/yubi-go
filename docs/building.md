# Building YUBI Go from source

Most users do not need this page, because they can install the APK from the
[latest release](https://github.com/airoa-org/yubi-go/releases/latest) instead
([`README.md`](../README.md)).

## The airoa_core payload

The app runs AIRoA's `airoa_core` inside a WASM component, `airoa-sdk/build/airoa-core.wasm`.
The component is committed, because it is built from AIRoA's `airoa_data_collection` repository,
which is not public. `android/build.sh` compiles it for the headset, and that step needs
nothing private. `airoa-sdk/build/PAYLOAD` pins the upstream commits, and
`airoa-core.wasm.src` stamps the sources that the component was built from. If you change
`airoa-sdk/python/`, the WIT files, the lock files or `PAYLOAD`, the build rebuilds the
component, so it then needs read access to `airoa_data_collection`.

## Prerequisites

- Rust 1.95 or newer (`rust-version` in `Cargo.toml`) with the `aarch64-linux-android` target,
  and `cargo-ndk`.
- OpenJDK 17.
- An Android SDK with build-tools 33.0.0, platform 33 and NDK 27.2.12479018. Set
  `$ANDROID_HOME` to it, because `android/build.sh` has no fallback path.
- `uv` and `wkg 0.16.1` (`cargo install wkg --version 0.16.1 --locked`), only to rebuild the
  payload component (see above).
- `cargo-about`, which writes the licence texts that the APK carries as `LICENSES.txt`.
- `zip`, `unzip`, `curl` and `shasum`, which `android/build.sh` and `tools/tts/fetch.sh` call.

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk
cargo install --locked cargo-about --features cli
```

On macOS with Homebrew, also set `JAVA_HOME=/opt/homebrew/opt/openjdk@17` and put
`/opt/homebrew/opt/rustup/bin` on `PATH` before `~/.cargo/bin`.

## Build and install

```bash
export ANDROID_HOME="$HOME/Android/Sdk"        # macOS: ~/Library/Android/sdk
./android/build.sh
adb install -r target/apk/yubi-go.apk
adb shell pm grant org.airoa.yubigo android.permission.CAMERA   # once per headset
adb shell pm grant org.airoa.yubigo horizonos.permission.USB_CAMERA
adb shell am start -n org.airoa.yubigo/android.app.NativeActivity
adb logcat -s yubigo
```

The build downloads the Rust crates and the speech engine, so it needs the network.

`build.sh` signs the APK with your machine's debug key (`~/.android/debug.keystore`), which it
creates on the first build. Android installs an update only over an APK signed with the same
key. So to replace the release APK with your own build, copy
`/sdcard/Android/data/org.airoa.yubigo/files/` off the headset, then `adb uninstall
org.airoa.yubigo`, because uninstalling deletes the episodes.

Release APKs are signed with AIRoA's release key instead: `build.sh` uses it when
`YUBI_GO_KEYSTORE` names the keystore and `YUBI_GO_KEYSTORE_PASS` holds its password.

A release attaches `target/licenses/LICENSES.txt` beside `yubi-go.apk`. It is the same file
that the APK carries and that `:8080/licenses` serves.

## Tests

```bash
cargo build-cwasm
cargo test --workspace
```

The host tests need a host build of the `airoa_core` payload, so run `cargo build-cwasm` with no
target argument first. Some modules compile for Android only, so the host tests do not cover them.
[`CONTRIBUTING.md`](../CONTRIBUTING.md) lists the other checks.
