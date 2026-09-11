#!/usr/bin/env bash
# cw-media-demo.sh — D1, federated media: play a title that lives on another
# member's machine, from this machine, with no VPN and no port forwarded.
#
# Two roles, three lines each.
#
#   HOLDER (the machine with the library):
#     scripts/cw-media-demo.sh holder-up        # Jellyfin on 127.0.0.1:8096 + one test title + wizard done
#     # add to ~/.svrnmesh/config.toml under [iroh]:   media_origin = "127.0.0.1:8096"
#     svrn daemon stop; svrn daemon start       # the acceptor now advertises cwth/media/0 to members
#
#   VIEWER (any other member):
#     svrn mesh media <holder-name>             # prints http://127.0.0.1:NNNNN + path (direct/relayed) + a probe
#     # open that URL in a browser (Jellyfin web) or point a player at it; log in as demo / demo
#
# Jellyfin's own web UI and API are what the viewer talks to — the bridge is a
# byte splice and knows nothing about media. Nothing here is exposed beyond
# loopback on either machine; the only way in is a key the roster carries.
#
# `holder-down` stops the container and leaves the library + config on disk.
set -euo pipefail

ROOT="${CW_MEDIA_ROOT:-$HOME/.svrnmesh/media-demo}"
IMAGE="${CW_MEDIA_IMAGE:-docker.io/jellyfin/jellyfin:latest}"
NAME="cw-jellyfin"
ORIGIN="127.0.0.1:8096"
AUTH='MediaBrowser Client="cw-media-demo", Device="cli", DeviceId="cw-media-demo", Version="1"'

say() { printf '%s\n' "$*" >&2; }

api() { # method path [json]
  local m="$1" p="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -sS -f -X "$m" "http://$ORIGIN$p" -H "Content-Type: application/json" -H "X-Emby-Authorization: $AUTH" -d "$body"
  else
    curl -sS -f -X "$m" "http://$ORIGIN$p" -H "X-Emby-Authorization: $AUTH"
  fi
}

holder_up() {
  command -v podman >/dev/null || { say "podman is required on the holder (Fedora host: it is there; the sovereign-vulkan toolbox: it is not)"; exit 3; }
  mkdir -p "$ROOT/media" "$ROOT/config" "$ROOT/cache"

  if [ ! -s "$ROOT/media/Commonwealth Test Pattern (2026).mp4" ]; then
    say "generating a 2-minute 1080p test title with the image's own ffmpeg…"
    podman run --rm --entrypoint /usr/lib/jellyfin-ffmpeg/ffmpeg \
      -v "$ROOT/media:/media:Z" "$IMAGE" -v error \
      -f lavfi -i "testsrc2=size=1920x1080:rate=30" -f lavfi -i "sine=frequency=440:sample_rate=48000" \
      -t 120 -c:v libx264 -preset veryfast -b:v 8M -pix_fmt yuv420p -c:a aac -b:a 128k \
      "/media/Commonwealth Test Pattern (2026).mp4"
  fi

  if podman container exists "$NAME"; then
    podman start "$NAME" >/dev/null
  else
    podman run -d --name "$NAME" -p "$ORIGIN:8096" \
      -v "$ROOT/media:/media:Z" -v "$ROOT/config:/config:Z" -v "$ROOT/cache:/cache:Z" \
      "$IMAGE" >/dev/null
  fi

  say "waiting for Jellyfin on $ORIGIN…"
  # Kestrel accepts (and resets) before the app answers, so a single 200 is
  # not "ready": require the public-info document itself, twice in a row.
  local info="" ok=0
  for _ in $(seq 1 90); do
    if info="$(curl -sS -f --max-time 3 "http://$ORIGIN/System/Info/Public" 2>/dev/null)" \
       && [[ "$info" == *StartupWizardCompleted* ]]; then
      ok=$((ok + 1)); [ "$ok" -ge 2 ] && break
    else
      ok=0
    fi
    sleep 1
  done
  [ "$ok" -ge 2 ] || { say "Jellyfin did not come up on $ORIGIN within 90 s — podman logs $NAME"; exit 4; }
  say "jellyfin: $info"

  if echo "$info" | grep -q '"StartupWizardCompleted":false'; then
    say "running the first-run wizard (user demo / demo, library /media)…"
    api POST /Startup/Configuration '{"UICulture":"en-US","MetadataCountryCode":"US","PreferredMetadataLanguage":"en"}' >/dev/null
    api GET  /Startup/User >/dev/null
    api POST /Startup/User '{"Name":"demo","Password":"demo"}' >/dev/null
    api POST "/Library/VirtualFolders?name=Movies&collectionType=movies&refreshLibrary=true" \
      '{"LibraryOptions":{"PathInfos":[{"Path":"/media"}],"EnableRealtimeMonitor":false}}' >/dev/null
    api POST /Startup/RemoteAccess '{"EnableRemoteAccess":false,"EnableAutomaticPortMapping":false}' >/dev/null
    api POST /Startup/Complete >/dev/null
    say "wizard complete."
  fi

  cat >&2 <<EOF

holder is up: http://$ORIGIN (loopback only; login demo / demo)
library:      $ROOT/media

declare it to the mesh — in ~/.svrnmesh/config.toml under [iroh]:
  media_origin = "$ORIGIN"
then:  svrn daemon stop; svrn daemon start
a member then runs:  svrn mesh media <this node's name>
EOF
}

holder_down() {
  podman stop "$NAME" >/dev/null 2>&1 && say "stopped $NAME (library and config kept under $ROOT)" || say "$NAME was not running"
}

case "${1:-}" in
  holder-up)   holder_up ;;
  holder-down) holder_down ;;
  *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
