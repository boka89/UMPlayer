#!/usr/bin/env bash
# Starts real Jellyfin, Plex and Navidrome servers in Docker with a few generated albums
# and checks that Unitn can sign in, browse and stream from them.
set -u
MUSIC=/tmp/srvmusic
mkdir -p "$MUSIC"

make_album() { # artist album year ext tracks...
  local artist="$1" album="$2" year="$3" ext="$4"; shift 4
  local dir="$MUSIC/$artist/$album"; mkdir -p "$dir"
  ffmpeg -loglevel error -y -f lavfi -i "color=c=#e8663d:s=500x500" -frames:v 1 "$dir/cover.jpg"
  local n=1
  for title in "$@"; do
    ffmpeg -loglevel error -y -f lavfi -i "sine=frequency=$((220 + n * 110)):duration=20" -ac 2 \
      -metadata title="$title" -metadata artist="$artist" -metadata album_artist="$artist" \
      -metadata album="$album" -metadata date="$year" -metadata track="$n" \
      "$dir/$(printf %02d $n) $title.$ext"
    n=$((n + 1))
  done
}
make_album "Duna Quartet" "Danube Sessions" 2018 flac "Buda" "Pest" "Margaret Island"
make_album "Aurora Lane" "Northern Lights" 2021 mp3 "Polar Night" "Glass Rivers"

wait_for() { # url seconds
  for _ in $(seq 1 "$2"); do curl -sf "$1" >/dev/null && return 0; sleep 1; done
  return 1
}

status=0

# ---------------- Jellyfin ----------------
echo "::group::Jellyfin setup"
docker run -d --name jellyfin --network host -v "$MUSIC:/media:ro" jellyfin/jellyfin:latest >/dev/null
JF=http://localhost:8096
wait_for "$JF/System/Info/Public" 180 || echo "Jellyfin did not start"
sleep 5
H='Content-Type: application/json'
curl -sf -X POST "$JF/Startup/Configuration" -H "$H" -d '{"UICulture":"en-US","MetadataCountryCode":"US","PreferredMetadataLanguage":"en"}' || true
curl -sf "$JF/Startup/User" >/dev/null || true
curl -sf -X POST "$JF/Startup/User" -H "$H" -d '{"Name":"unitn","Password":"secret123"}' || true
curl -sf -X POST "$JF/Startup/RemoteAccess" -H "$H" -d '{"EnableRemoteAccess":true,"EnableAutomaticPortMapping":false}' || true
curl -sf -X POST "$JF/Startup/Complete" || true
AUTH='MediaBrowser Client="ci", Device="ci", DeviceId="ci-setup", Version="1"'
TOKEN=$(curl -sf -X POST "$JF/Users/AuthenticateByName" -H "$H" -H "Authorization: $AUTH" \
  -d '{"Username":"unitn","Pw":"secret123"}' | jq -r .AccessToken)
echo "token: ${TOKEN:0:4}…"
JA="Authorization: $AUTH, Token=\"$TOKEN\""
curl -s -o /dev/null -w "add library: %{http_code}\n" -X POST \
  "$JF/Library/VirtualFolders?name=Music&collectionType=music&paths=%2Fmedia&refreshLibrary=true" \
  -H "$H" -H "$JA" -d '{"LibraryOptions":{"PathInfos":[{"Path":"/media"}]}}'
sleep 3
curl -s -o /dev/null -w "refresh: %{http_code}\n" -X POST "$JF/Library/Refresh" -H "$JA"
echo "libraries: $(curl -s "$JF/Library/VirtualFolders" -H "$JA" | jq -c '[.[] | {Name, Locations}]')"
docker exec jellyfin ls -la /media || true
for _ in $(seq 1 90); do
  n=$(curl -sf "$JF/Items?IncludeItemTypes=MusicAlbum&Recursive=true" -H "Authorization: $AUTH, Token=\"$TOKEN\"" | jq -r .TotalRecordCount)
  [ "${n:-0}" -ge 2 ] 2>/dev/null && break
  sleep 2
done
echo "albums indexed: ${n:-?}"
docker logs jellyfin 2>&1 | grep -iE "error|warn|scan" | tail -15
echo "::endgroup::"
UNITN_SAVE_SERVER=/tmp/jellyfin.json \
  cargo run --release -q -p unitn-core --example server_check -- jellyfin localhost unitn secret123 || status=1

# ---------------- Plex ----------------
echo "::group::Plex setup"
mkdir -p /tmp/plexcfg
docker run -d --name plex --network host -e TZ=UTC -v /tmp/plexcfg:/config -v "$MUSIC:/music:ro" plexinc/pms-docker:latest >/dev/null
PX=http://localhost:32400
wait_for "$PX/identity" 180 || echo "Plex did not start"
PH=(-H 'Accept: application/json' -H 'X-Plex-Client-Identifier: unitn-ci' -H 'X-Plex-Product: Unitn CI')
echo "identity: $(curl -s "$PX/identity")"
echo "sections before: $(curl -s -w ' [%{http_code}]' "${PH[@]}" "$PX/library/sections" | head -c 300)"
for attempt in 1 2 3 4 5 6; do
  for agent in tv.plex.agents.music com.plexapp.agents.lastfm com.plexapp.agents.none; do
    scanner=$([ "$agent" = tv.plex.agents.music ] && echo "Plex%20Music" || echo "Plex%20Music%20Scanner")
    code=$(curl -s -o /tmp/plex-create.txt -w "%{http_code}" -X POST "${PH[@]}" \
      "$PX/library/sections?name=Music&type=artist&agent=$agent&scanner=$scanner&language=en-US&location=%2Fmusic")
    echo "create library (attempt $attempt, $agent): $code $(head -c 200 /tmp/plex-create.txt)"
    [ "$code" = 200 ] || [ "$code" = 201 ] && break 2
  done
  sleep 10
done
SECTION=$(curl -s "${PH[@]}" "$PX/library/sections" | jq -r '[.MediaContainer.Directory[]? | select(.type=="artist") | .key][0] // empty')
echo "music section: ${SECTION:-none}"
[ -n "$SECTION" ] && curl -s -o /dev/null -X GET "${PH[@]}" "$PX/library/sections/$SECTION/refresh"
for _ in $(seq 1 90); do
  [ -n "$SECTION" ] || break
  n=$(curl -s "${PH[@]}" "$PX/library/sections/$SECTION/all?type=9" | jq -r '.MediaContainer.size // 0')
  [ "${n:-0}" -ge 2 ] 2>/dev/null && break
  sleep 2
done
echo "albums indexed: ${n:-?}"
echo "::endgroup::"
# An unclaimed server trusts local requests, so any token works here.
cargo run --release -q -p unitn-core --example server_check -- plex localhost "" local-test-token || status=1

# ---------------- Navidrome (OpenSubsonic) ----------------
echo "::group::Navidrome setup"
docker run -d --name navidrome --network host -e ND_MUSICFOLDER=/music -e ND_LOGLEVEL=info \
  -v "$MUSIC:/music:ro" deluan/navidrome:latest >/dev/null
ND=http://localhost:4533
wait_for "$ND/ping" 120 || echo "Navidrome did not start"
curl -s -o /dev/null -w "create admin: %{http_code}\n" -X POST "$ND/auth/createAdmin" \
  -H 'Content-Type: application/json' -d '{"username":"unitn","password":"secret123"}'
SQ="u=unitn&p=secret123&v=1.16.1&c=ci&f=json"
curl -s "$ND/rest/startScan.view?$SQ" | jq -c '."subsonic-response".status' || true
for _ in $(seq 1 60); do
  n=$(curl -s "$ND/rest/getAlbumList2.view?$SQ&type=alphabeticalByName&size=50" | jq -r '[."subsonic-response".albumList2.album[]?] | length')
  [ "${n:-0}" -ge 2 ] 2>/dev/null && break
  sleep 2
done
echo "albums indexed: ${n:-?}"
echo "::endgroup::"
cargo run --release -q -p unitn-core --example server_check -- subsonic localhost unitn secret123 || status=1

exit $status
