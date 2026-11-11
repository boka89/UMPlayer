#!/usr/bin/env bash
# Plays a local AAC file, a WAV file and an internet radio stream with the real
# app on a macOS runner (with a virtual sound card) and checks that playback
# actually starts and the position advances.
set -uo pipefail
BIN="$PWD/target/release/unitn"

brew install --cask blackhole-2ch >/dev/null 2>&1 || echo "BlackHole install failed"
sudo killall coreaudiod 2>/dev/null || true
sleep 5
system_profiler SPAudioDataType | sed -n 1,30p

python3 - <<'PY'
import math, struct, wave
w = wave.open("smoke.wav", "wb"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(44100)
w.writeframes(b"".join(struct.pack("<hh", int(8000*math.sin(2*math.pi*440*i/44100)),
                                   int(8000*math.sin(2*math.pi*660*i/44100))) for i in range(44100*20)))
w.close()
PY
afconvert -f m4af -d aac smoke.wav smoke.m4a
python3 - <<'PY'
import math, struct, wave
for name, f in (("gap1.wav", 330), ("gap2.wav", 550)):
    w = wave.open(name, "wb"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(44100)
    w.writeframes(b"".join(struct.pack("<hh", int(8000*math.sin(2*math.pi*f*i/44100)), 0) for i in range(44100*4)))
    w.close()
PY

fail=0
run() { # name target seconds
  echo "=================== $1: $2"
  UNITN_AUTOPLAY="$2" "$BIN" > "out-$1.txt" 2>&1 &
  local pid=$!
  sleep "${3:-10}"
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
  cat "out-$1.txt"
  if grep -q "State(Playing)" "out-$1.txt" && grep -qE "position: [1-9]" "out-$1.txt"; then
    echo "RESULT $1: OK"
  else
    echo "RESULT $1: FAIL"; fail=1
  fi
}
run m4a "$PWD/smoke.m4a" 10
run wav "$PWD/smoke.wav" 10
run radio "https://icecast.radiofrance.fr/fip-midfi.mp3" 15
run oxygen "https://oxygenmusic.hu:8443/oxygenmusic" 15

# Gapless: two 4 s files in a row must chain without stopping.
echo "=================== gapless"
UNITN_AUTOPLAY="$PWD/gap1.wav,$PWD/gap2.wav" "$BIN" > out-gap.txt 2>&1 &
pid=$!
sleep 12
kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
grep -E "audio:|Advanced|Ended" out-gap.txt
if grep -q "gapless switch" out-gap.txt && grep -q "Advanced" out-gap.txt; then
  echo "RESULT gapless: OK"
else
  echo "RESULT gapless: FAIL"; fail=1
fi

# Radio first, then a local file: the file must still play.
echo "=================== sequence: radio then file"
UNITN_AUTOPLAY="https://oxygenmusic.hu:8443/oxygenmusic|$PWD/smoke.m4a" "$BIN" > out-seq.txt 2>&1 &
pid=$!
sleep 18
screencapture -x window.png 2>/dev/null || true
kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
cat out-seq.txt
if grep -q 'load File' out-seq.txt && [ "$(grep -c 'State(Playing)' out-seq.txt)" -ge 1 ] \
   && awk '/load File/{f=1} f && /position: [1-9]/{ok=1} END{exit !ok}' out-seq.txt; then
  echo "RESULT sequence: OK"
else
  echo "RESULT sequence: FAIL"; fail=1
fi

# Frosted glass: the UI must stay visible on top of the blur. Screenshots go
# to the ci-mac-screenshots branch.
mkdir -p mac-screenshots
CONF="$HOME/Library/Application Support/net.bokors.Unitn"
mkdir -p "$CONF"
shot_with() { # name settings-json
  echo "$2" > "$CONF/settings.json"
  UNITN_AUTOPLAY="$PWD/smoke.m4a" "$BIN" > "out-$1.txt" 2>&1 &
  local pid=$!
  sleep 8
  screencapture -x "mac-screenshots/$1.png" 2>/dev/null || echo "screencapture failed"
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
  grep -E "frosted glass|panic|error" "out-$1.txt" || true
}
echo "=================== frosted glass"
shot_with glass-off '{"theme": 2, "volume": 0.5}'
shot_with glass-old-setting '{"theme": 2, "volume": 0.5, "glass": true}'
shot_with glass-on-dark '{"theme": 2, "volume": 0.5, "frosted_glass": true}'
shot_with glass-on-light '{"theme": 1, "volume": 0.5, "frosted_glass": true, "opacity": 0.8}'
# Second round: the CI's virtual display sometimes garbles a capture.
shot_with glass-off-2 '{"theme": 2, "volume": 0.5}'
shot_with glass-old-setting-2 '{"theme": 2, "volume": 0.5, "glass": true}'
shot_with glass-on-dark-2 '{"theme": 2, "volume": 0.5, "frosted_glass": true}'
shot_with glass-on-light-2 '{"theme": 1, "volume": 0.5, "frosted_glass": true}'
echo "=================== startup animation and window size"
# The splash plays, then the main window opens at the size saved last time.
echo '{"theme": 2, "volume": 0.3, "window_width": 1100.0, "window_height": 700.0}' > "$CONF/settings.json"
UNITN_NO_INTRO= "$BIN" > out-intro.txt 2>&1 &
pid=$!
sleep 2.5; screencapture -x mac-screenshots/intro-splash.png || true
sleep 6; screencapture -x mac-screenshots/intro-after.png || true
kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
grep -E "intro|startup|panic|error" out-intro.txt || true
if grep -q "intro: .* frames shown" out-intro.txt && grep -q "startup: window shown" out-intro.txt && ! grep -q PANIC out-intro.txt; then
  echo "RESULT intro: OK"
else
  echo "RESULT intro: FAIL"; fail=1
fi
grep -o '"window_width":[^,]*,"window_height":[^,}]*' "$CONF/settings.json" || grep -E 'window_(width|height)' "$CONF/settings.json" || true
ls -la mac-screenshots
exit $fail
