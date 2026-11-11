#!/usr/bin/env bash
# Starts Unitn in a virtual X display with generated sample music and
# captures screenshots of the main pages in dark and light theme.
set -euo pipefail

BIN="$PWD/target/release/unitn"
OUT="$PWD/screenshots"
mkdir -p "$OUT"

# --- sample library -------------------------------------------------------
MUSIC="$HOME/Music"
make_album() { # artist album year color tracks...
  local artist="$1" album="$2" year="$3" color="$4"; shift 4
  local dir="$MUSIC/$artist/$album"; mkdir -p "$dir"
  ffmpeg -loglevel error -y -f lavfi -i "color=c=$color:s=500x500" -frames:v 1 "$dir/cover.jpg"
  local n=1
  for title in "$@"; do
    # Pink noise plus a chord, so the spectrum visualizer has something to show.
    local f=$((110 + n * 27)) d=$((150 + n * 23))
    ffmpeg -loglevel error -y \
      -f lavfi -i "anoisesrc=color=pink:amplitude=0.12:duration=$d" \
      -f lavfi -i "aevalsrc=0.22*sin(2*PI*$f*t)+0.16*sin(2*PI*$f*1.5*t)+0.12*sin(2*PI*$f*2*t)+0.08*sin(2*PI*$f*4.02*t)+0.05*sin(2*PI*$f*8.1*t):s=44100:d=$d" \
      -i "$dir/cover.jpg" \
      -filter_complex "[0:a][1:a]amix=inputs=2:normalize=0[a]" \
      -map "[a]" -map 2:v -c:a libmp3lame -q:a 6 -c:v copy -id3v2_version 3 \
      -metadata title="$title" -metadata artist="$artist" -metadata album="$album" \
      -metadata date="$year" -metadata track="$n" -disposition:v attached_pic "$dir/$(printf %02d $n) $title.mp3"
    n=$((n + 1))
  done
}
make_album "Aurora Lane" "Northern Lights" 2021 "#6a46f5" "Polar Night" "Glass Rivers" "Snowfall Radio" "Midnight Sun"
make_album "The Copper Kites" "Paper Skies" 2019 "#e8663d" "Lift Off" "Cardboard Hearts" "Weather Report"
make_album "Mira Solano" "Café Noir" 2023 "#2a2a2a" "Espresso" "Rue de la Paix" "Late Show"
make_album "Static Bloom" "Neon Gardens" 2022 "#1fb58f" "Photosynthesis" "Chlorophyll" "Night Garden" "Overgrowth"
make_album "Duna Quartet" "Danube Sessions" 2018 "#3d7be8" "Buda" "Pest" "Margaret Island"
make_album "Kalimba Kids" "Sunday Morning" 2024 "#f2c230" "Pancakes" "Garden Hose"

# An album without any cover, to exercise the online album art lookup
# (Ghosts I–IV is released under a Creative Commons license).
dir="$MUSIC/Nine Inch Nails/Ghosts I–IV"; mkdir -p "$dir"
ffmpeg -loglevel error -y -f lavfi -i "sine=frequency=330:duration=120" -c:a libmp3lame -q:a 7 \
  -metadata title="1 Ghosts I" -metadata artist="Nine Inch Nails" -metadata album="Ghosts I–IV" \
  -metadata date=2008 -metadata track=1 "$dir/01 Ghosts I.mp3"

# Synced lyrics for the first song, as a .lrc file next to it.
cat > "$MUSIC/Aurora Lane/Northern Lights/01 Polar Night.lrc" <<'LRC'
[ar:Aurora Lane]
[ti:Polar Night]
[00:00.50]Snow is falling on the frozen bay
[00:04.00]Every lantern keeps the dark away
[00:08.00]Polar night, hold on to me
[00:12.00]Northern lights across the sea
[00:16.00]Glass rivers sleeping under ice
[00:20.00]Midnight sun will come, it will come twice
[00:24.00]Polar night, hold on to me
[00:28.00]Northern lights across the sea
[00:32.00]
[00:36.00]Count the stars until the morning
[00:40.00]Count the stars until the morning
LRC

mkdir -p "$HOME/.config"
echo "XDG_MUSIC_DIR=\"$MUSIC\"" > "$HOME/.config/user-dirs.dirs"

# --- run ----------------------------------------------------------------------
SETTINGS="$HOME/.config/unitn/settings.json"
mkdir -p "$(dirname "$SETTINGS")"

shot() { import -window root "$OUT/$1.png"; }
click() { xdotool mousemove "$1" "$2" click 1; sleep "${3:-1.5}"; }

run_theme() { # theme-number name
  local servers=""
  # Show a friendly server name instead of the CI machine's host name.
  [ -f /tmp/jellyfin.json ] && servers=", \"servers\": [$(jq -c '.name = "Home Jellyfin"' /tmp/jellyfin.json)]"
  echo "{\"theme\": $1, \"volume\": 0.8, \"glass\": false$servers}" > "$SETTINGS"
  if [ "$2" = dark ]; then
    # This run shows the startup animation.
    UNITN_NO_INTRO= "$BIN" > "$OUT/log-$2.txt" 2>&1 &
  else
    "$BIN" > "$OUT/log-$2.txt" 2>&1 &
  fi
  local pid=$!
  if [ "$2" = dark ]; then
    for _ in $(seq 50); do
      WID=$(xdotool search --name Unitn | head -1 || true)
      [ -n "$WID" ] && break
      sleep 0.1
    done
    xdotool windowmove "$WID" 0 0 windowsize "$WID" 1280 820 || true
    sleep 1.4; shot "$2-intro-1"
    sleep 1.6; shot "$2-intro-2"
    sleep 1.8; shot "$2-intro-3"
    sleep 4
  else
    sleep 8
  fi
  WID=$(xdotool search --name Unitn | head -1)
  xdotool windowmove "$WID" 0 0 windowsize "$WID" 1280 820
  sleep 2
  shot "$2-albums"
  click 341 160            # first album card
  shot "$2-album"
  if [ "$2" = dark ]; then
    click 714 257 1        # album "…" menu
    shot "$2-album-menu"
    click 622 436 8        # Fix album info… (searches MusicBrainz)
    shot "$2-album-fix"
    xdotool key Escape; sleep 1
    click 850 660 1        # Cancel, in case Escape did not arrive
  fi
  click 866 254 1          # Cozy song list
  shot "$2-album-cozy"
  click 790 254 1          # back to Comfy
  click 524 257 4          # Play album
  shot "$2-playing"
  click 120 117 2          # Albums: the playing album is marked
  shot "$2-albums-playing"
  click 120 411 3          # Now Playing (visualizer)
  shot "$2-nowplaying"
  xdotool mousemove 900 480; sleep 0.8   # mouse over the visualizer: its buttons appear
  shot "$2-nowplaying-controls"
  sleep 3.2
  shot "$2-nowplaying-idle"  # mouse resting: the buttons are gone again
  xdotool mousemove 1215 326; sleep 0.6
  click 1221 326 3         # full screen button (the buttons fade out when the mouse rests)
  shot "$2-viz-0"
  xdotool mousemove 700 400; sleep 0.6
  shot "$2-viz-controls"   # mouse moved: buttons and hints are back
  for i in 1 2 3; do
    xdotool mousemove 700 420; sleep 0.6
    click 1209 37 0.5      # the › button (next style)
    xdotool mousemove 640 450; sleep 3.2   # rest: buttons fade out
    shot "$2-viz-$i"
  done
  xdotool windowfocus "$WID" 2>/dev/null || true
  xdotool key Right; sleep 2  # arrow key: next style
  shot "$2-viz-4"
  xdotool key Escape; sleep 1
  click 1240 40 1          # close button, in case Escape did not arrive
  click 1187 92 2          # Equalizer button on Now Playing
  shot "$2-nowplaying-eq"
  click 359 255 1          # presets menu
  shot "$2-eq-presets"
  click 427 406 1          # Rock
  shot "$2-nowplaying-eq-rock"
  click 297 255 1          # ON/off: back to bypassed
  click 1187 92 1          # close it again
  xdotool key Escape; sleep 1
  click 1070 92 3          # Lyrics button (synced .lrc next to the song)
  shot "$2-lyrics"
  click 1070 92 1          # back to the visualizer
  click 120 285 6          # Radio (wait for stations)
  shot "$2-radio"
  if [ "$2" = dark ]; then
    click 700 168 10       # play first station
    shot "$2-radio-playing"
    click 120 411 3
    shot "$2-nowplaying-radio"
    click 120 327 2        # Podcasts
    shot "$2-podcasts-empty"
    click 1110 42 1        # search box
    xdotool type --delay 60 "NASA"; sleep 8
    shot "$2-podcasts-search"
    click 1190 140 12      # Subscribe to the first show
    shot "$2-podcasts-subscribed"
    click 1110 42 1
    xdotool key ctrl+a BackSpace; sleep 3
    shot "$2-podcasts"
    click 500 130 8        # open the show
    shot "$2-podcast-show"
    click 600 250 12       # play the newest episode
    shot "$2-podcast-playing"
    click 120 411 3
    shot "$2-nowplaying-podcast"
    sleep 14                 # progress is saved every 15 s
    click 120 327 2          # back to the show: progress bar and "… left"
    shot "$2-podcast-progress"
    click 120 486 1          # somewhere else (the server), then play the episode again
    click 120 327 2
    click 600 250 10         # same episode: continues where it was
    shot "$2-podcast-resume"
  fi
  click 120 201            # Songs
  shot "$2-songs"
  click 200 697            # Settings (gear next to the theme switch)
  shot "$2-settings"
  if [ -f /tmp/jellyfin.json ]; then
    click 120 486 6        # the Jellyfin server in the sidebar
    shot "$2-server"
    click 341 206 4        # first album on the server
    shot "$2-server-album"
    click 524 257 4        # play it
    click 120 486 3        # back to the server: the album is marked
    shot "$2-server-playing"
  fi
  if [ "$2" = dark ]; then
    # Try to make the window tiny: it must stay at the minimum size, with
    # the now playing bar visible.
    xdotool windowsize "$WID" 480 360
    sleep 3
    shot "$2-small-window"
    xdotool getwindowgeometry "$WID" | tee "$OUT/small-window.txt"
  fi
  kill "$pid" || true
  wait "$pid" 2>/dev/null || true
}

# Virtual sound card so playback can be exercised.
pulseaudio -D --exit-idle-time=-1 --system=false || true
sleep 1
pactl load-module module-null-sink sink_name=null || true
pactl set-default-sink null || true

# Crossfade on the real (null) sound device: two 14 s tones, 4 s overlap.
ffmpeg -loglevel error -y -f lavfi -i "sine=frequency=330:duration=14" -ac 2 /tmp/cf-a.wav
ffmpeg -loglevel error -y -f lavfi -i "sine=frequency=550:duration=14" -ac 2 /tmp/cf-b.wav
cargo run --release -q -p unitn-core --example crossfade_check -- /tmp/cf-a.wav /tmp/cf-b.wav 4 \
  | tee "$OUT/crossfade.txt" || true

# "Sign in with Plex": create a real sign-in request and capture plex.tv's page for it.
if cargo run --release -q -p unitn-core --example plex_pin_check -- /tmp/plex-auth-url.txt | tee "$OUT/plex-pin.txt"; then
  for chrome in google-chrome google-chrome-stable chromium chromium-browser; do
    if command -v "$chrome" >/dev/null; then
      timeout 60 "$chrome" --headless=new --no-sandbox --disable-gpu --hide-scrollbars --window-size=1280,900 \
        --virtual-time-budget=15000 --screenshot="$OUT/plex-signin-page.png" "$(cat /tmp/plex-auth-url.txt)" 2>/dev/null || true
      break
    fi
  done
fi

export DISPLAY=:99
Xvfb :99 -screen 0 1280x820x24 &
XVFB=$!
sleep 2
run_theme 2 dark
run_theme 1 light
kill $XVFB || true
ls -la "$OUT"
