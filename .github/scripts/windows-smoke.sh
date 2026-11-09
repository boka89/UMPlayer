#!/usr/bin/env bash
# Starts the Windows release build and reports whether it stays running.
set -uo pipefail
python - <<'PY'
import math, struct, wave
w = wave.open("smoke.wav", "wb"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(44100)
w.writeframes(b"".join(struct.pack("<hh", int(8000*math.sin(2*math.pi*440*i/44100)), 0) for i in range(44100*10)))
w.close()
PY
LOG="$APPDATA/bokors/Unitn/data/unitn.log"
UNITN_AUTOPLAY="$(cygpath -w "$PWD/smoke.wav")" ./target/release/unitn.exe &
pid=$!
sleep 15
if kill -0 "$pid" 2>/dev/null; then
  echo "RESULT windows: still running after 15 s"
  status=0
  taskkill //F //IM unitn.exe >/dev/null 2>&1 || kill "$pid"
else
  wait "$pid"; code=$?
  echo "RESULT windows: EXITED with code $code"
  status=1
fi
echo "----- unitn.log -----"
cat "$LOG" 2>/dev/null || echo "(no log at $LOG)"
if grep -q "PANIC" "$LOG" 2>/dev/null; then echo "RESULT windows: PANIC in log"; status=1; fi
grep -q "State(Playing)" "$LOG" 2>/dev/null && echo "RESULT windows: playback started" || echo "RESULT windows: no playback (no audio device on the runner?)"
exit $status
