#!/usr/bin/env bash
# install.sh — stand up the on-prem legal pilot from an untarred kit.
#
#   tar --zstd -xf firm-rag-<version>.tar.zst
#   cd firm-rag-<version>
#   sudo ./install.sh --docs /srv/firm-docs --hostname firm-rag.example.com
#
# Target: ~1 hour of an IT person's time, reading one page. Everything
# this script does is one of: create a directory, copy a file, write a
# config, enable a unit, or call the daemon it just started on loopback.
# It contacts NOTHING else. If it ever appears to hang on the network,
# that is a bug — see EGRESS.md.
#
# ── Why there is no `svrn setup` here ────────────────────────────────
# The setup wizard fetches GGUFs from HuggingFace and prompts on a TTY.
# On an air-gapped box it cannot do either, so this script writes the
# config by hand instead. That is why daemon-config.toml is commented so
# heavily: it is the wizard's replacement, and nothing validates it (the
# schema rejects no unknown key).
#
# ── One process, two phases ──────────────────────────────────────────
# The daemon (sovereign-onprem) is started UNKEYED first, on loopback
# only, to restore the legal corpus and register the document share:
# the restore's embedding probe and the register call carry no API key.
# Then it is stopped, the keys and the corpus allow-list are written, and
# it is started KEYED. From then on every caller, loopback included,
# presents a key.
#
# ── --no-systemd: a sandbox install ──────────────────────────────────
# Installs as the invoking user, into the --prefix/--data/--etc given,
# with no service account, no /etc/systemd and no /etc/nginx. The unit
# file is written under <prefix>/systemd and checked with
# `systemd-analyze verify`; the nginx files under <prefix>/nginx. The
# daemon is started by running the unit's own ExecStart with its own
# Environment= lines, so the sandbox runs what the unit would. Used to
# prove a kit on a host that must not be touched (pick --port away from
# the host's own daemon).
#
# Idempotent: safe to re-run. It will not overwrite an existing config
# or regenerate API keys unless you pass --force-config.

set -euo pipefail

KIT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PREFIX=/opt/firm-rag
DATA=/var/lib/firm-rag
ETC=/etc/firm-rag
SVC_USER=firmrag
PORT=9741
DOCS_DIR=""
HOSTNAME_FQDN=""
DAEMON_CONFIG_SRC="$KIT_DIR/config/daemon-config.toml"
FORCE_CONFIG=0
SKIP_ACCEPTANCE=0
NO_SYSTEMD=0

die() { printf 'install: %s\n' "$*" >&2; exit 1; }
say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

usage() {
    cat <<'EOF'
usage: sudo ./install.sh --docs <path> --hostname <fqdn> [options]

  --docs <path>       the mounted share holding the firm's documents.
                      Mounted READ-ONLY into the daemon's sandbox — this
                      system indexes their files, it never writes to them.
  --hostname <fqdn>   the TLS hostname lawyers will use.

  --prefix <path>     binaries          (default /opt/firm-rag)
  --data <path>       models + indexes  (default /var/lib/firm-rag)
  --etc <path>        config + keys     (default /etc/firm-rag)
  --user <name>       service account   (default firmrag)
  --port <n>          the client API port (default 9741). The internal
                      port is n+1, the unused cw-rails base n+6 and
                      serve's loopback port n+7, as 9742/9747/9748 are.
  --daemon-config <f> install this daemon config instead of the kit's
                      (a different model profile); it must keep the
                      kit's paths, ports and markers
  --force-config      overwrite the existing config AND regenerate API keys
  --skip-acceptance   install but do not run acceptance.sh
  --no-systemd        sandbox install as the invoking user (see header)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --docs)     DOCS_DIR="${2:-}"; shift 2 ;;
        --hostname) HOSTNAME_FQDN="${2:-}"; shift 2 ;;
        --prefix)   PREFIX="${2:-}"; shift 2 ;;
        --data)     DATA="${2:-}"; shift 2 ;;
        --etc)      ETC="${2:-}"; shift 2 ;;
        --user)     SVC_USER="${2:-}"; shift 2 ;;
        --port)     PORT="${2:-}"; shift 2 ;;
        --daemon-config) DAEMON_CONFIG_SRC="${2:-}"; shift 2 ;;
        --force-config)    FORCE_CONFIG=1; shift ;;
        --skip-acceptance) SKIP_ACCEPTANCE=1; shift ;;
        --no-systemd)      NO_SYSTEMD=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage; die "unknown argument: $1" ;;
    esac
done

if [ "$NO_SYSTEMD" -eq 1 ]; then
    SVC_USER="$(id -un)"
else
    [ "$(id -u)" -eq 0 ] || die "must run as root (systemd unit, /etc, service user)"
fi
[ -n "$DOCS_DIR" ]      || { usage; die "--docs is required"; }
[ -n "$HOSTNAME_FQDN" ] || { usage; die "--hostname is required"; }
[ -d "$DOCS_DIR" ]      || die "--docs path does not exist: $DOCS_DIR"
[ -f "$DAEMON_CONFIG_SRC" ] || die "no daemon config at $DAEMON_CONFIG_SRC"
for tool in curl jq sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || die "\`$tool\` is not installed"
done
case "$PORT" in ''|*[!0-9]*) die "--port must be a number, got '$PORT'" ;; esac
DOCS_DIR="$(cd "$DOCS_DIR" && pwd)"
DAEMON_URL="http://127.0.0.1:$PORT"
UNIT_NAME=firm-rag.service

# ── 0. Verify the kit before trusting a byte of it ───────────────────
# The whole point of an air-gapped delivery is that nothing was fetched
# at install time; that only means something if the thing on the USB
# stick is the thing we built.
say "verifying kit integrity"
if [ -f "$KIT_DIR/MANIFEST.sha256" ]; then
    ( cd "$KIT_DIR" && sha256sum -c --quiet MANIFEST.sha256 ) \
        || die "MANIFEST.sha256 does not match. Do NOT install this kit."
    echo "    manifest OK"
else
    die "no MANIFEST.sha256 in $KIT_DIR — refusing to install an unverifiable kit.
     (If you are deliberately installing a hand-assembled kit, generate one:
      cd $KIT_DIR && find . -type f ! -name MANIFEST.sha256 -exec sha256sum {} + > MANIFEST.sha256)"
fi

# ── 1. Service account ───────────────────────────────────────────────
if [ "$NO_SYSTEMD" -eq 1 ]; then
    say "service account: none (--no-systemd runs as $SVC_USER)"
else
    say "service account: $SVC_USER"
    if ! id -u "$SVC_USER" >/dev/null 2>&1; then
        # No login shell, no home worth having. HOME is set in the unit file
        # to a directory under $DATA so any home-derived path in the process
        # lands somewhere writable.
        useradd --system --no-create-home --home-dir "$DATA" --shell /usr/sbin/nologin "$SVC_USER"
        echo "    created"
    else
        echo "    exists"
    fi
fi

# Run a command as the service account (the daemon must own what it reads).
as_svc() {
    if [ "$NO_SYSTEMD" -eq 1 ]; then "$@"; else sudo -u "$SVC_USER" "$@"; fi
}

# Every `svrn` call names its config and its daemon explicitly: a verb
# whose config does not load falls back to port 9741, which on a shared
# host is somebody else's daemon. SVRNMESH_DATA_DIR makes
# $DATA/config.toml (linked to the installed config below) the file it
# reads; the env is otherwise empty so nothing inherited can redirect it.
svrn() {
    as_svc env -i PATH=/usr/bin:/bin HOME="$DATA" \
        SVRNMESH_DATA_DIR="$DATA" SOVEREIGN_DATA_DIR="$DATA" \
        SOVEREIGN_DAEMON_URL="$DAEMON_URL" SOVEREIGN_NO_STALE_WARN=1 \
        "$PREFIX/bin/svrn" "$@"
}

# ── 2. Directories ───────────────────────────────────────────────────
say "directories"
own=(-o "$SVC_USER" -g "$SVC_USER")
etc_own=(-o root -g "$SVC_USER")
[ "$NO_SYSTEMD" -eq 1 ] && etc_own=("${own[@]}")
install -d -m 0755 "$PREFIX/bin"
install -d -m 0750 "${own[@]}" "$DATA"
install -d -m 0750 "${own[@]}" "$DATA/models"
install -d -m 0750 "${own[@]}" "$DATA/lib"
install -d -m 0750 "${etc_own[@]}" "$ETC"

# ── 3. Binaries ──────────────────────────────────────────────────────
# The on-prem distribution, and the CLI siblings this script's verbs
# exec. `svrn` is the sovereign-cli dispatcher; it resolves its siblings
# by exact filename next to its own path, so all of them land in the
# same directory and keep their names.
say "binaries → $PREFIX/bin"
for b in sovereign-onprem svrn sovereign-cli-daemon svrn-ingest; do
    [ -f "$KIT_DIR/bin/$b" ] || die "kit is missing bin/$b"
    install -m 0755 "$KIT_DIR/bin/$b" "$PREFIX/bin/$b"
    echo "    $b"
done

# ── 4. Models ────────────────────────────────────────────────────────
say "models → $DATA/models"
for f in "$KIT_DIR"/models/*.gguf; do
    [ -e "$f" ] || die "kit contains no models/*.gguf"
    install -m 0640 "${own[@]}" "$f" "$DATA/models/$(basename "$f")"
    echo "    $(basename "$f")"
done

# ── 5. OCR assets ────────────────────────────────────────────────────
# Both halves are required and they fail differently. Missing models:
# the daemon logs `ocr:unavailable reason=models_not_found` at boot and
# scanned PDFs are reported as scanned_no_text. Missing libpdfium: the
# daemon warns but installs the context anyway, and OCR then produces
# nothing at all, because no PDF can be rasterized. The second is the
# quieter failure, which is why it gets the same hard check here.
say "OCR assets → $DATA/models/paddle-ocr, $DATA/lib"
if [ -d "$KIT_DIR/ocr/paddle-ocr" ]; then
    cp -a "$KIT_DIR/ocr/paddle-ocr" "$DATA/models/"
    [ "$NO_SYSTEMD" -eq 1 ] || chown -R "$SVC_USER:$SVC_USER" "$DATA/models/paddle-ocr"
    set_dir="$DATA/models/paddle-ocr/ppocr-en-v4v5"
    for f in det.onnx rec.onnx dict.txt; do
        [ -f "$set_dir/$f" ] || die "OCR model set is incomplete: $set_dir/$f is missing.
     A partial set does not half-work — the engine refuses at ingest."
    done
    install -m 0644 "${own[@]}" "$KIT_DIR/ocr/libpdfium.so" "$DATA/lib/libpdfium.so" \
        || die "kit is missing ocr/libpdfium.so. Without it no PDF can be rasterized and OCR yields nothing."
    echo "    paddle-ocr + libpdfium.so"
else
    echo "    SKIPPED — no ocr/ in the kit. Scanned PDFs will be reported as"
    echo "    scanned_no_text and will not be indexed. For a litigation"
    echo "    practice this is usually the wrong tradeoff; see README.md."
fi

# ── 6. Config ────────────────────────────────────────────────────────
# The kit's paths and ports, rewritten to this install's. The ports move
# together so a sandbox install never names a port of the host's own
# daemon, cw-rails or serve.
say "config → $ETC"
place() {
    sed -e "s|/var/lib/firm-rag|$DATA|g" \
        -e "s|/etc/firm-rag|$ETC|g" \
        -e "s|/opt/firm-rag|$PREFIX|g" \
        -e "s|/srv/firm-docs|$DOCS_DIR|g" \
        -e "s|^client_port = 9741$|client_port = $PORT|" \
        -e "s|^internal_port = 9742$|internal_port = $((PORT + 1))|" \
        -e "s|^rails_base = \"http://127.0.0.1:9747\"$|rails_base = \"http://127.0.0.1:$((PORT + 6))\"|" \
        -e "s|^Environment=SOVEREIGN_SERVE_PORT=9748$|Environment=SOVEREIGN_SERVE_PORT=$((PORT + 7))|" \
        "$1"
}
CONFIG="$ETC/daemon-config.toml"
FRESH_CONFIG=0
if [ -f "$CONFIG" ] && [ "$FORCE_CONFIG" -eq 0 ]; then
    echo "    $CONFIG exists — kept (pass --force-config to overwrite)"
else
    grep -q '^# __INSTALL_CORPORA__$' "$DAEMON_CONFIG_SRC" \
        || die "$DAEMON_CONFIG_SRC has no '# __INSTALL_CORPORA__' marker under [retrieval].
     Refusing to guess where the corpus allow-list goes: on a keyed daemon an
     allow-list that lands outside [retrieval] parses cleanly and grants nothing."
    place "$DAEMON_CONFIG_SRC" > "$CONFIG"
    [ "$NO_SYSTEMD" -eq 1 ] || chown root:"$SVC_USER" "$CONFIG"
    chmod 0640 "$CONFIG"
    FRESH_CONFIG=1
    echo "    $(basename "$CONFIG")"
fi
grep -q "^client_port = $PORT$" "$CONFIG" \
    || die "$CONFIG does not set client_port = $PORT. The kit's config must keep
     'client_port = 9741' for --port to move it."
# The `svrn` verbs read <data dir>/config.toml; one file, two names.
ln -sfn "$CONFIG" "$DATA/config.toml"
svrn daemon key --list >/dev/null \
    || die "\`svrn daemon key --list\` could not load $DATA/config.toml.
     Every svrn call below would talk to the wrong daemon. Fix the config first."
echo "    $DATA/config.toml → $CONFIG (the svrn verbs' view, checked loadable)"

if [ ! -f "$ETC/acceptance-probes.env" ]; then
    install -m 0640 "${etc_own[@]}" \
        "$KIT_DIR/config/acceptance-probes.env.template" "$ETC/acceptance-probes.env"
    echo "    acceptance-probes.env (TEMPLATE — must be filled in, see below)"
fi

# ── 7. systemd ───────────────────────────────────────────────────────
say "systemd unit"
if [ "$NO_SYSTEMD" -eq 1 ]; then
    UNIT_DIR="$PREFIX/systemd"
    install -d -m 0755 "$UNIT_DIR"
else
    UNIT_DIR=/etc/systemd/system
fi
UNIT="$UNIT_DIR/$UNIT_NAME"
place "$KIT_DIR/systemd/$UNIT_NAME" \
    | sed -e "s|^User=.*|User=$SVC_USER|" -e "s|^Group=.*|Group=$SVC_USER|" > "$UNIT"
echo "    $UNIT"
# Main's two units: firm-rag-server.service retired with sovereign-server,
# firm-rag-daemon.service is firm-rag.service now. Left enabled, either
# binds the daemon's port beside the new unit. Run alone by
# quality/xtask/tests/onprem_kit_upgrade_unit.rs.
# retire-main-units: begin
for old in firm-rag-server.service firm-rag-daemon.service; do
    [ -f "$UNIT_DIR/$old" ] || continue
    [ "$NO_SYSTEMD" -eq 1 ] || systemctl disable --now "$old" 2>/dev/null || true
    rm -f "$UNIT_DIR/$old"
    echo "    retired $old"
done
# retire-main-units: end
if command -v systemd-analyze >/dev/null 2>&1; then
    systemd-analyze verify "$UNIT" \
        || die "systemd-analyze verify rejected $UNIT"
    echo "    systemd-analyze verify: clean"
else
    echo "    systemd-analyze is not installed — the unit was NOT verified"
fi
[ "$NO_SYSTEMD" -eq 1 ] || systemctl daemon-reload

# ── 8. nginx ─────────────────────────────────────────────────────────
say "nginx"
if [ "$NO_SYSTEMD" -eq 1 ]; then
    NGINX_SNIPPETS="$PREFIX/nginx"; NGINX_CONF="$PREFIX/nginx"
else
    NGINX_SNIPPETS=/etc/nginx/snippets; NGINX_CONF=/etc/nginx/conf.d
fi
install -d -m 0755 "$NGINX_SNIPPETS" "$NGINX_CONF"
# The snippets name each other by their installed path; one rewrite serves
# every file, so a sandbox prefix never includes a snippet under /etc.
snippet_paths=(-e "s|/etc/nginx/snippets/|$NGINX_SNIPPETS/|g")
install -m 0644 "$KIT_DIR/nginx/firm-rag-proxy-headers.conf" "$NGINX_SNIPPETS/firm-rag-proxy-headers.conf"
sed "${snippet_paths[@]}" "$KIT_DIR/nginx/firm-rag-proxy.conf" > "$NGINX_SNIPPETS/firm-rag-proxy.conf"
chmod 0644 "$NGINX_SNIPPETS/firm-rag-proxy.conf"
sed -e "s|firm-rag\.example\.com|$HOSTNAME_FQDN|g" \
    -e "s|server 127\.0\.0\.1:9741;|server 127.0.0.1:$PORT;|" \
    "${snippet_paths[@]}" \
    "$KIT_DIR/nginx/firm-rag.conf" > "$NGINX_CONF/firm-rag.conf"
echo "    $NGINX_CONF/firm-rag.conf (server_name $HOSTNAME_FQDN, upstream :$PORT)"
echo "    certs are NOT installed by this script — see step 1 below"

# ── Starting and stopping the one process ────────────────────────────
PIDFILE="$DATA/firm-rag.pid"
start_daemon() {
    if [ "$NO_SYSTEMD" -eq 0 ]; then
        systemctl enable --now "$UNIT_NAME"
        return
    fi
    # The unit's own command line and environment, minus systemd's sandbox.
    local exec_start
    exec_start="$(sed -n 's/^ExecStart=//p' "$UNIT")"
    local -a envs=()
    while IFS= read -r kv; do envs+=("$kv"); done < <(sed -n 's/^Environment=//p' "$UNIT")
    # shellcheck disable=SC2086 # ExecStart is a plain argv with no quoting
    env -i PATH=/usr/bin:/bin "${envs[@]}" $exec_start >> "$DATA/firm-rag.log" 2>&1 &
    echo $! > "$PIDFILE"
}
stop_daemon() {
    if [ "$NO_SYSTEMD" -eq 0 ]; then
        systemctl stop "$UNIT_NAME"
        return
    fi
    local pid
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    [ -n "$pid" ] || return 0
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    rm -f "$PIDFILE"
}
# Readiness is GET /health answering `ok` — unauthenticated, nothing else.
# Models load on first use, so this is "the API is up", not "loaded";
# acceptance check 1 reads residency.
wait_ready() {
    printf '    waiting for %s/health' "$DAEMON_URL"
    for _ in $(seq 1 120); do
        if [ "$(curl -s --max-time 5 "$DAEMON_URL/health" 2>/dev/null)" = "ok" ]; then
            echo; return 0
        fi
        if [ "$NO_SYSTEMD" -eq 1 ] && ! kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then
            echo; die "the daemon exited during boot: tail -50 $DATA/firm-rag.log"
        fi
        printf '.'; sleep 5
    done
    echo
    die "the daemon did not answer /health in 10 minutes.
     journalctl -u firm-rag -n 100   (or $DATA/firm-rag.log under --no-systemd)
     The daemon refuses to start rather than starting degraded, so the
     log names the reason. Most likely: a GGUF path in daemon-config.toml
     that does not exist (the VRAM preflight labels the slot UNREADABLE),
     or a port another process holds."
}
keyed() { [ -n "$(ls -A "$DATA/client-tokens" 2>/dev/null)" ]; }

# ── 9. Keys first, then the corpus steps under IT's key ──────────────
# The kit's config declares `[daemon] loopback = "none"`, so the daemon
# never runs without keys: they are issued here with no daemon running
# (`svrn daemon key` edits the store directly then), and the share is
# registered presenting IT's admin key. Until 2026-10-08 this phase ran
# the daemon unkeyed on loopback, which a declared `none` forbids.
if keyed && [ "$FORCE_CONFIG" -eq 0 ]; then
    say "keys exist — corpus restore and share registration were done by an earlier run"
else
    # Stopped first: with a daemon listening, `svrn daemon key` asks it
    # instead of editing the store, and this daemon would refuse it.
    stop_daemon
    rm -rf "$DATA/client-tokens"
    say "API keys → $DATA/client-tokens (the daemon's key store)"
    issue() {
        local out key
        out="$(svrn daemon key --add "$@")" || die "svrn daemon key --add $* failed: $out"
        key="$(printf '%s\n' "$out" | sed -n 's/^  \([^ ][^ ]*\)$/\1/p' | head -n1)"
        [ -n "$key" ] || die "svrn daemon key --add $* printed no key: $out"
        printf '%s\t%s\n' "$1" "$key" >> "$ETC/issued-keys.txt"
    }
    install -m 0600 /dev/null "$ETC/issued-keys.txt"
    issue firm                 # the lawyers' key: conversations, documents, corpora
    issue it --group admin     # IT's key: ingest and the admin routes
    svrn daemon key --list | sed 's/^/    /'
    echo "    issued → $ETC/issued-keys.txt (mode 0600)"
    IT_KEY="$(sed -n 's/^it\t//p' "$ETC/issued-keys.txt")"

    say "starting the daemon KEYED, on loopback, for the corpus steps"
    start_daemon
    wait_ready
    CORPORA=()

    say "restoring the us-code corpus"
    if [ -f "$KIT_DIR/corpora/us-code.tar.zst" ]; then
        sha=""
        [ -f "$KIT_DIR/corpora/us-code.sha256" ] && sha="$(cut -d' ' -f1 < "$KIT_DIR/corpora/us-code.sha256")"
        # Restore HARD-ERRORS on an embedding-dimension mismatch. That is
        # the good outcome: it means this box runs a different embed model
        # than the one that built the snapshot, and a silent restore would
        # return quietly wrong neighbours forever.
        svrn corpus snapshot restore \
            --archive "$KIT_DIR/corpora/us-code.tar.zst" \
            --as us-code \
            --into "$DATA/indexes" \
            ${sha:+--expected-sha256 "$sha"} \
            || { stop_daemon; die "snapshot restore failed — see the message above.
     An embedding-dimension mismatch means the embed model in
     daemon-config.toml is not the one that built this snapshot."; }
        CORPORA+=(us-code)
        echo "    us-code restored"
    else
        echo "    SKIPPED — no corpora/us-code.tar.zst in the kit"
    fi

    # The share is a watched folder: walked recursively and kept in sync
    # every 5 minutes, scanned PDFs read by OCR. The daemon mints the
    # corpus id from the share's path; it is read back, never guessed.
    # The first sweep runs inside the register call (`sync_initial`), so
    # the corpus exists and is listed when the keyed daemon starts —
    # without it the first sweep waits for the scheduler and acceptance
    # finds an empty corpus. On a large share this is the slow step.
    say "watching the document share: $DOCS_DIR (first sweep; a large share takes a while)"
    reg="$(curl -sS -X POST "$DAEMON_URL/internal/corpus/watch/register" \
        -H 'Content-Type: application/json' \
        -H "Authorization: Bearer $IT_KEY" \
        --data "$(jq -nc --arg p "$DOCS_DIR" \
            '{path: $p, display_name: "firm-docs", config: {with_ocr: true, sweep_interval_secs: 300}, sync_initial: true}')")" \
        || { stop_daemon; die "could not reach $DAEMON_URL to register the share"; }
    docs_id="$(printf '%s' "$reg" | jq -r '.corpus_id // empty')"
    [ -n "$docs_id" ] || { stop_daemon; die "the daemon did not register the share: $reg"; }
    CORPORA+=("$docs_id")
    echo "    corpus $docs_id, first sweep $(printf '%s' "$reg" | jq -c '.initial_sweep')"
    stop_daemon

    # ── 10. The allow-list ───────────────────────────────────────────
    say "corpus allow-list → [retrieval] in $CONFIG"
    list="$(printf '"%s", ' "${CORPORA[@]}")"
    list="corpora = [${list%, }]"
    if grep -q '^# __INSTALL_CORPORA__$' "$CONFIG"; then
        sed -i "s|^# __INSTALL_CORPORA__$|$list|" "$CONFIG"
    elif [ "$FRESH_CONFIG" -eq 0 ]; then
        sed -i "s|^corpora = \[.*\]$|$list|" "$CONFIG"
    fi
    # Prove it landed in the right table rather than trusting the sed.
    awk '/^\[retrieval\]/{f=1;next} /^\[/{f=0} f' "$CONFIG" | grep -qxF "$list" \
        || die "the allow-list is not inside [retrieval] after substitution.
     A keyed daemon would grant every key nothing."
    echo "    $list"
fi

# ── 11. Start keyed ──────────────────────────────────────────────────
say "starting the daemon KEYED"
start_daemon
wait_ready
code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$DAEMON_URL/v1/corpora")"
[ "$code" = "401" ] || die "GET /v1/corpora with no key answered $code, not 401.
     The daemon did not load its keys — it is serving loopback as the owner."
echo "    a request with no key is refused (401)"

# ── Done ─────────────────────────────────────────────────────────────
cat <<EOF

$(say "installed")

Four things remain, and the box is not ready until all four are done:

  1. TLS certificates. Put the real cert and key at:
       /etc/ssl/firm-rag/fullchain.pem
       /etc/ssl/firm-rag/privkey.pem
     then:  nginx -t && systemctl reload nginx

  2. Fill in $ETC/acceptance-probes.env.
     Checks 3, 4 and 5 CANNOT be judged without it, and acceptance.sh
     will exit 2 rather than pretend they passed. The values must come
     from the firm's own practice area and their own scans — see the
     comments in that file.

  3. Hand out the lawyers' key from $ETC/issued-keys.txt (the 'firm'
     line; root-readable only). IT keeps the 'it' key. Each key owns its
     own conversations; every key retrieves from the same corpora.

  4. Run the acceptance suite against the daemon AND the TLS hostname:
       BASE_URL=https://$HOSTNAME_FQDN DAEMON_URL=$DAEMON_URL \\
         API_KEY=<firm key> ADMIN_KEY=<it key> $KIT_DIR/acceptance.sh
     Gate on the exit code: 0 ready, 1 failed, 2 could-not-judge.

EOF

if [ "$SKIP_ACCEPTANCE" -eq 0 ]; then
    say "running acceptance now (expect UNSURE until steps 1-2 are done)"
    API_KEY="$(sed -n 's/^firm\t//p' "$ETC/issued-keys.txt")" \
    ADMIN_KEY="$(sed -n 's/^it\t//p' "$ETC/issued-keys.txt")" \
    BASE_URL="https://$HOSTNAME_FQDN" DAEMON_URL="$DAEMON_URL" DATA_DIR="$DATA" \
    PROBES_FILE="$ETC/acceptance-probes.env" DAEMON_CONFIG="$CONFIG" \
        "$KIT_DIR/acceptance.sh" || true
fi
