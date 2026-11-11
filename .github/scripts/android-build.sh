#!/usr/bin/env bash
# Builds the experimental Android app: dist/android/Unitn_<version>_android.apk
#
# Needs: the Android SDK (ANDROID_HOME) with build-tools, a platform and the
# NDK; a JDK (javac, keytool); Rust with the Android targets; cargo-ndk.
# No Gradle: the few Java classes are compiled with javac and d8 directly.
#   rustup target add aarch64-linux-android x86_64-linux-android
#   cargo install cargo-ndk
#
# ABIS="x86_64" builds for one architecture only (default: phones + emulator).
# UNITN_ANDROID_KEYSTORE (a .p12 or .jks file) and
# UNITN_ANDROID_KEYSTORE_PASSWORD sign with the permanent key; without them a
# throw-away test key is made. (Android only installs an update over an app
# signed with the same key, so releases must always use the permanent one.)
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
mkdir -p "$STAGE/lib" "$STAGE/res/mipmap-xxxhdpi" "$STAGE/classes" "$STAGE/dex"

# --- Java (activity + playback service) -------------------------------------
# First, because it takes seconds and the Rust build takes minutes.
javac -source 8 -target 8 -Xlint:-options -nowarn \
  -bootclasspath "$PLATFORM/android.jar" -d "$STAGE/classes" \
  $(find crates/unitn-android/java -name '*.java')
"$BUILD_TOOLS/d8" --release --min-api "$MIN_SDK" --lib "$PLATFORM/android.jar" \
  --output "$STAGE/dex" $(find "$STAGE/classes" -name '*.class')
ls -la "$STAGE/dex"

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
(cd "$STAGE/dex" && zip -q ../unsigned.apk classes.dex)
"$BUILD_TOOLS/zipalign" -f -p 4 "$STAGE/unsigned.apk" "$STAGE/aligned.apk"

# --- sign -------------------------------------------------------------------
KEYSTORE=${UNITN_ANDROID_KEYSTORE:-}
PASSWORD=${UNITN_ANDROID_KEYSTORE_PASSWORD:-android}
if [ -z "$KEYSTORE" ]; then
  KEYSTORE="$STAGE/test.keystore"
  keytool -genkeypair -keystore "$KEYSTORE" -storepass "$PASSWORD" -keypass "$PASSWORD" \
    -alias unitn -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Unitn test build" 2>/dev/null
  echo "RESULT android signing: throw-away test key"
else
  echo "RESULT android signing: permanent key"
fi
APK="$OUT/Unitn_${VERSION}_android.apk"
"$BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass "pass:$PASSWORD" --out "$APK" "$STAGE/aligned.apk"
"$BUILD_TOOLS/apksigner" verify --print-certs "$APK" | grep -i "SHA-256" | sed 's/^/RESULT android /'
rm -f "$APK.idsig"
ls -la "$APK"
echo "RESULT android apk: OK ($(du -h "$APK" | cut -f1))"
