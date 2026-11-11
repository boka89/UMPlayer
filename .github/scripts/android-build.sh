#!/usr/bin/env bash
# Builds the experimental Android app: dist/android/Unitn_<version>_android.apk
#
# Needs: the Android SDK (ANDROID_HOME) with build-tools, a platform and the
# NDK; a JDK (javac, keytool); Rust with the Android targets; cargo-ndk.
#   rustup target add aarch64-linux-android x86_64-linux-android
#   cargo install cargo-ndk
#
# ABIS="x86_64" builds for one architecture only (default: phones + emulator).
# UNITN_ANDROID_KEYSTORE / UNITN_ANDROID_KEYSTORE_PASSWORD sign with your own
# key; otherwise a throw-away test key is made. (Android only installs an
# update over an app signed with the same key.)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"

ABIS=${ABIS:-"arm64-v8a x86_64"}
MIN_SDK=26      # Android 8.0, the oldest version Slint supports
TARGET_SDK=34
OUT="$ROOT/dist/android"
STAGE="$OUT/stage"

: "${ANDROID_HOME:?Set ANDROID_HOME to the Android SDK folder}"
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-${ANDROID_NDK_LATEST_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}}
BUILD_TOOLS=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)
PLATFORM="$ANDROID_HOME/platforms/android-$TARGET_SDK"
[ -f "$PLATFORM/android.jar" ] || PLATFORM=$(ls -d "$ANDROID_HOME"/platforms/android-[0-9][0-9] | sort -V | tail -1)
# Slint compiles a small Java helper against the SDK's android.jar. Point it
# at the platform directly: cargo-ndk sets ANDROID_PLATFORM to the minimum
# API level, and that platform is usually not installed in the SDK.
export ANDROID_JAR="$PLATFORM/android.jar"
echo "NDK:         $ANDROID_NDK_HOME"
echo "build-tools: $BUILD_TOOLS"
echo "platform:    $PLATFORM"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
IFS=. read -r major minor patch <<<"$VERSION"
VERSION_CODE=$((major * 10000 + minor * 100 + patch))

rm -rf "$STAGE"
mkdir -p "$STAGE/lib" "$STAGE/res/mipmap-xxxhdpi"

# --- native library ---------------------------------------------------------
# Start from the desktop lock file, so both builds use the same crate versions;
# cargo only adds what Android needs on top.
cp Cargo.lock crates/unitn-android/Cargo.lock
targets=()
for abi in $ABIS; do targets+=(-t "$abi"); done
extra=()
# Newer cargo-ndk versions can link clang's builtins (128-bit float helpers
# that SQLite needs on x86_64).
if cargo ndk --help 2>/dev/null | grep -q -- '--link-builtins'; then extra+=(--link-builtins); fi
(
  cd crates/unitn-android
  cargo ndk "${targets[@]}" --platform "$MIN_SDK" ${extra[@]+"${extra[@]}"} -o "$STAGE/lib" build --release
)
cp crates/unitn-android/Cargo.lock "$OUT/Cargo.lock"
find "$STAGE/lib" -type f -exec ls -la {} +

# --- package ----------------------------------------------------------------
cp crates/unitn/icons/256x256.png "$STAGE/res/mipmap-xxxhdpi/ic_launcher.png"
"$BUILD_TOOLS/aapt2" compile --dir "$STAGE/res" -o "$STAGE/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$STAGE/unsigned.apk" \
  -I "$PLATFORM/android.jar" \
  --manifest crates/unitn-android/AndroidManifest.xml \
  --min-sdk-version "$MIN_SDK" --target-sdk-version "$TARGET_SDK" \
  --version-code "$VERSION_CODE" --version-name "$VERSION" \
  "$STAGE/res.zip"
(cd "$STAGE" && zip -q -r unsigned.apk lib)
"$BUILD_TOOLS/zipalign" -f -p 4 "$STAGE/unsigned.apk" "$STAGE/aligned.apk"

# --- sign -------------------------------------------------------------------
KEYSTORE=${UNITN_ANDROID_KEYSTORE:-}
PASSWORD=${UNITN_ANDROID_KEYSTORE_PASSWORD:-android}
if [ -z "$KEYSTORE" ]; then
  KEYSTORE="$STAGE/test.keystore"
  keytool -genkeypair -keystore "$KEYSTORE" -storepass "$PASSWORD" -keypass "$PASSWORD" \
    -alias unitn -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Unitn test build" 2>/dev/null
  echo "Signed with a throw-away test key."
fi
APK="$OUT/Unitn_${VERSION}_android.apk"
"$BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass "pass:$PASSWORD" --out "$APK" "$STAGE/aligned.apk"
"$BUILD_TOOLS/apksigner" verify "$APK"
rm -f "$APK.idsig"
ls -la "$APK"
echo "RESULT android apk: OK ($(du -h "$APK" | cut -f1))"
