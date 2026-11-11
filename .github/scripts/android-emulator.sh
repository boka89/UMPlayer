#!/usr/bin/env bash
# Runs inside a booted Android emulator (CI): installs the APK, starts the
# app with a radio stream, and saves screenshots and the app's log to
# android-screenshots/. Read the "RESULT …" lines.
set -uo pipefail

APK=$(ls dist/android/Unitn_*_android.apk | head -1)
OUT="$PWD/android-screenshots"
STREAM=${1:-https://oxygenmusic.hu:8443/oxygenmusic}
ACTIVITY=net.bokors.unitn/android.app.NativeActivity
mkdir -p "$OUT"

shot() { adb exec-out screencap -p > "$OUT/$1.png" && echo "screenshot: $1.png"; }
applog() { adb logcat -d -s unitn:V 2>/dev/null; }
result() { # name pattern
  if applog | grep -q "$2"; then echo "RESULT android $1: OK"; else echo "RESULT android $1: FAILED"; FAILED=1; fi
}
FAILED=0

adb shell wm size | tee "$OUT/device.txt"
adb shell wm density | tee -a "$OUT/device.txt"
adb shell getprop ro.build.version.release | tee -a "$OUT/device.txt"

adb install -r "$APK" || { echo "RESULT android install: FAILED"; exit 1; }
echo "RESULT android install: OK"

# Test hooks (see crates/unitn/src/android.rs).
adb shell setprop debug.unitn.no_intro 1
adb shell setprop debug.unitn.autoplay "$STREAM"
adb logcat -c
adb shell am start -W -n "$ACTIVITY"

# Wait for the app to start and the stream to play for a few seconds.
for _ in $(seq 1 45); do
  sleep 2
  if [ "$(applog | grep -c 'ui: event Position')" -ge 5 ]; then break; fi
done
shot tablet-playing

result "startup" "startup: ready"
result "audio output" "audio: output device"
result "radio playback" "ui: event Position"
if adb shell pidof net.bokors.unitn >/dev/null; then echo "RESULT android still running: OK"; else echo "RESULT android still running: FAILED"; FAILED=1; fi

# Touch: open Radio and Now Playing from the sidebar (positions in screen
# pixels on the 2560x1800 tablet). Only pictures, never a failure.
adb shell input tap 155 615 && sleep 8 && shot tablet-radio
adb shell input tap 196 865 && sleep 4 && shot tablet-nowplaying

# The desktop layout on a phone-sized screen (expected to be cramped: there
# is no phone layout yet). Only a picture, never a failure.
adb shell wm size 1080x2400 && adb shell wm density 420 && sleep 6 && shot phone-portrait
adb shell wm size reset; adb shell wm density reset; sleep 3

# The start-up animation.
adb shell am force-stop net.bokors.unitn
adb shell setprop debug.unitn.no_intro '""'
adb shell setprop debug.unitn.autoplay '""'
adb shell am start -W -n "$ACTIVITY"
sleep 2
shot tablet-intro
sleep 10
shot tablet-idle

applog > "$OUT/logcat-unitn.txt"
adb logcat -d '*:E' 2>/dev/null | grep -i -E "unitn|FATAL|DEBUG|libc|AndroidRuntime" | tail -200 > "$OUT/logcat-errors.txt"
echo "--- app log (first 80 lines) ---"
head -80 "$OUT/logcat-unitn.txt"
if [ "$FAILED" = 1 ]; then
  echo "--- errors ---"
  tail -60 "$OUT/logcat-errors.txt"
fi
exit "$FAILED"
