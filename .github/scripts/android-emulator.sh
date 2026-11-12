#!/usr/bin/env bash
# Runs inside a booted Android emulator (CI): installs the APK and checks
#   - local songs: found in the Music folder and played,
#   - background playback: the music goes on (and moves to the next song)
#     after Home is pressed, with the notification and media buttons working,
#   - internet radio, and the hand-over to the browser (Plex sign-in),
# and saves phone- and tablet-sized screenshots plus the app's log to
# android-screenshots/. Read the "RESULT …" lines.
set -uo pipefail

APK=$(ls dist/android/Unitn_*_android.apk | head -1)
OUT="$PWD/android-screenshots"
SONGS=${SONGS:-test-songs}
STREAM=${STREAM:-https://oxygenmusic.hu:8443/oxygenmusic}
APP=net.bokors.unitn
ACTIVITY=$APP/.MainActivity
mkdir -p "$OUT"
FAILED=0

shot() { adb exec-out screencap -p > "$OUT/$1.png" && echo "screenshot: $1.png"; }
applog() { adb logcat -d -s unitn:V 2>/dev/null; }
count() { applog | grep -c "$1"; }
ok() { echo "RESULT android $1: OK"; }
fail() { echo "RESULT android $1: FAILED"; FAILED=1; }
check() { if "${@:2}" >/dev/null 2>&1; then ok "$1"; else fail "$1"; fi; }
logged() { applog | grep -q "$1"; }
wait_for() { # pattern, at least N times, timeout seconds
  for _ in $(seq 1 "$3"); do
    [ "$(count "$1")" -ge "$2" ] && return 0
    sleep 1
  done
  return 1
}
tap() { adb shell input tap "$1" "$2"; sleep "${3:-2}"; }
start_app() { adb shell am start -W -n "$ACTIVITY" >/dev/null; }
props() { # autoplay, no_intro, open_url ('""' clears a property)
  adb shell setprop debug.unitn.autoplay "${1:-\"\"}"
  adb shell setprop debug.unitn.no_intro "${2:-\"\"}"
  adb shell setprop debug.unitn.open_url "${3:-\"\"}"
}

adb shell getprop ro.build.version.release | tee "$OUT/device.txt"

# ---------------------------------------------------------------- phone size
adb shell wm size 1080x2400
adb shell wm density 420
sleep 4

# Test songs into the shared Music folder, and tell Android about them.
adb shell mkdir -p /sdcard/Music/TT/ES
for f in "$SONGS"/*.wav; do adb push "$f" /sdcard/Music/TT/ES/ >/dev/null; done
adb shell content call --method scan_volume --uri content://media --arg external_primary >/dev/null 2>&1
sleep 3

adb install -r "$APK" || { fail install; exit 1; }
ok install
# As if the user had answered "Allow" to the two questions.
adb shell pm grant $APP android.permission.READ_MEDIA_AUDIO
adb shell pm grant $APP android.permission.POST_NOTIFICATIONS

# Positions (screen pixels, 1080x2400 at 420 dpi): the five tabs are at
# x = 108, 324, 540, 756, 972 on y = 2200; the small player is at y = 2040.
TAB_Y=2200
savelog() { applog > "$OUT/logcat-$1.txt"; }

# --- 1. local songs, then into the background -------------------------------
props "/sdcard/Music/TT/ES/1.wav,/sdcard/Music/TT/ES/2.wav,/sdcard/Music/TT/ES/3.wav" 1
adb logcat -c
start_app
wait_for "ui: event Position" 4 60
sleep 2
shot phone-albums
check "startup" logged "startup: ready"
check "local song plays" logged "ui: event Position"
tap 300 2040 3          # the small player -> Now Playing
shot phone-nowplaying
# Now Playing buttons on y = 576: Like (x 145), Lyrics (407), Equalizer (700).
tap 145 576 3; shot phone-liked                               # Like: makes "Likes"
check "Like adds the song to Likes" logged "likes: added"

adb shell input keyevent KEYCODE_HOME
sleep 3
before=$(count "ui: event Position")
# The first song is 24 s long: wait for the second one, in the background.
wait_for "gapless switch\|crossfade over\|ui: event Advanced" 1 60
sleep 6
after=$(count "ui: event Position")
if [ "$after" -gt "$((before + 4))" ]; then ok "plays on in the background"; else fail "plays on in the background ($before -> $after)"; fi
check "next song starts in the background" logged "gapless switch\|crossfade over\|ui: event Advanced"
if adb shell dumpsys activity services $APP | grep -q "isForeground=true"; then ok "foreground service"; else fail "foreground service"; fi
adb shell dumpsys media_session > "$OUT/media-session.txt" 2>&1
if grep -A40 "$APP" "$OUT/media-session.txt" | grep -q "state=PlaybackState {state=\(3\|PLAYING\)"; then ok "media session playing"; else fail "media session playing"; fi
adb shell dumpsys notification --noredact 2>/dev/null | grep -A30 "pkg=$APP" | head -60 > "$OUT/notification.txt"
if grep -q "Second Wind\|First Light\|Third Coast" "$OUT/notification.txt"; then ok "notification shows the song"; else fail "notification shows the song"; fi
adb shell cmd statusbar expand-notifications; sleep 3
shot phone-notification
adb shell cmd statusbar collapse; sleep 1

# Headset / Bluetooth buttons.
adb shell input keyevent KEYCODE_MEDIA_PAUSE
if wait_for "ui: event State(Paused)" 1 10; then ok "media button pauses"; else fail "media button pauses"; fi
paused=$(count "ui: event State(Playing)")
adb shell input keyevent KEYCODE_MEDIA_PLAY
if wait_for "ui: event State(Playing)" "$((paused + 1))" 10; then ok "media button resumes"; else fail "media button resumes"; fi

# --- 2. back in the app: the other pages ------------------------------------
start_app; sleep 3
tap 108 $TAB_Y 3; tap 270 480 3; shot phone-album             # Albums -> first album
tap 324 $TAB_Y 3; shot phone-songs                            # Songs
tap 1006 281 2; shot phone-song-menu; tap 300 1500 1          # "…" menu, closed by a tap outside
tap 979 134 3; shot phone-search; tap 1010 134 2              # search box, close it
# Songs: long press picks a song (the "Add to Playlist…" bar appears above
# the list and moves the rows down), Back lets it go again. Then swipes:
# right = play next, left = end of the queue, each with a short bubble.
adb shell input swipe 500 423 500 423 900; sleep 1; shot phone-songs-picked
check "long press picks a song" logged "songs: 1 picked"
tap 733 260 2; shot phone-songs-add-to-playlist; tap 300 1900 1
adb shell input keyevent KEYCODE_BACK; sleep 1
adb shell input swipe 150 281 850 281 250; sleep 0.3; shot phone-songs-swiped-right
check "swipe right plays next" logged "songs: swiped right"
sleep 2
adb shell input swipe 850 423 150 423 250; sleep 0.3; shot phone-songs-swiped-left
check "swipe left adds to the end" logged "songs: swiped left"
sleep 1
tap 540 $TAB_Y 8; shot phone-radio                            # Radio
tap 584 252 1; tap 775 252 1; tap 938 252 8                  # filters: Hungary + Pop + Rock at once
shot phone-radio-filters
tap 127 252 6                                                 # Popular clears them again
tap 756 $TAB_Y 5; shot phone-podcasts                         # Podcasts
# "More" opens a menu above its tab: Library (y 1620), Listen (1746),
# Playlists (1872), and the theme switch with Settings (991, 2035).
tap 972 $TAB_Y 2; shot phone-more                             # More: its menu
tap 700 1746 3; shot phone-more-listen                        # -> Listen
tap 211 533 3; shot phone-queue                               # Listen -> Queue
tap 972 $TAB_Y 2; tap 700 1620 3; shot phone-more-library     # More -> Library
tap 211 407 3; shot phone-artists                             # Library -> Artists
tap 972 $TAB_Y 2; tap 700 1872 3; shot phone-more-playlists   # More -> Playlists (with Likes)
tap 972 $TAB_Y 2; tap 991 2035 3; shot phone-settings         # More -> Settings
adb shell input swipe 540 1700 540 500 400; sleep 2; shot phone-settings-2
adb shell input swipe 540 1700 540 500 400; sleep 2; shot phone-settings-3
tap 224 1485 3; shot phone-add-server                         # Settings -> Add server
tap 400 321 2; shot phone-add-server-plex                     # its Plex tab
adb shell input keyevent KEYCODE_BACK; sleep 2                # Back closes the dialog
# Another song plays by now: Like it, then tap again to take the like back.
tap 300 2040 3; tap 145 576 3; tap 145 576 3; shot phone-unliked
check "Like again takes the song out of Likes" logged "likes: removed"
tap 700 576 3; shot phone-equalizer                           # Equalizer
tap 700 576 1
# Back: Now Playing -> Albums, then out of the app (which keeps playing).
adb shell input keyevent KEYCODE_BACK; sleep 2; shot phone-after-back
adb shell input keyevent KEYCODE_BACK; sleep 3
if adb shell dumpsys activity activities | grep -m1 "ResumedActivity" | grep -q "$APP"; then fail "Back leaves the app"; else ok "Back leaves the app"; fi
check "still running after Back" adb shell pidof $APP
savelog local

# --- 3. internet radio + the browser hand-over ------------------------------
adb shell am force-stop $APP
props "$STREAM" 1
adb logcat -c
start_app
if wait_for "ui: event StreamTitle\|ui: event Position" 6 60; then ok "radio plays"; else fail "radio plays"; fi
sleep 3
tap 300 2040 4
shot phone-radio-playing
savelog radio

adb shell am force-stop $APP
props "" 1 "https://app.plex.tv/auth#?test=unitn"
adb logcat -c
start_app; sleep 6
if logged "open_url test: true"; then ok "opens the browser (Plex sign-in)"; else fail "opens the browser (Plex sign-in)"; fi
adb logcat -d 2>/dev/null | grep -m2 "android.intent.action.VIEW" | cut -c1-200
shot phone-browser
savelog browser
adb shell am force-stop $APP

# --- 4. the startup animation: the last frame must fit the screen -----------
intro_shot() {
  props
  adb logcat -c
  start_app; sleep 4     # 4.6 s of frames (the logo is complete by ~4 s), then a 0.9 s hold
  shot "$1-intro"
  sleep 3
  check "$1 startup animation" logged "intro: "
  adb shell am force-stop $APP
}
intro_shot phone

# --------------------------------------------------------------- tablet size
adb shell wm size reset; adb shell wm density reset; sleep 4
intro_shot tablet
props "$STREAM" 1
start_app
wait_for "ui: event Position" 4 60
shot tablet-playing
adb shell am force-stop $APP
props

savelog tablet
adb logcat -d '*:E' 2>/dev/null | grep -i -E "unitn|FATAL|AndroidRuntime|libc  " | tail -200 > "$OUT/logcat-errors.txt"
if [ "$FAILED" = 1 ]; then
  echo "--- app log, local songs run (without the position lines) ---"; grep -v "event Position" "$OUT/logcat-local.txt" | tail -80
  echo "--- errors ---"; tail -60 "$OUT/logcat-errors.txt"
fi
exit "$FAILED"
