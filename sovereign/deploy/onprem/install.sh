#!/usr/bin/env bash
# install.sh — stand up the on-prem legal pilot from an untarred kit.
#
#   tar --zstd -xf firm-rag-<version>.tar.zst
#   cd firm-rag-<version>
#   sudo ./install.sh --docs /srv/firm-docs --hostname firm-rag.example.com
#
# Target: ~1 hour of an IT person's time, reading one page. Everything
# this script does is one of: create a directory, copy a file, write a
# config, enable a unit. It contacts NOTHING. If it ever appears to hang
# on the network, that is a bug — see EGRESS.md.
#
# ── Why there is no `svrn setup` here ────────────────────────────────
# The setup wizard fetches GGUFs from HuggingFace and prompts on a TTY.
# On an air-gapped box it cannot do either, so this script writes both
# config files by hand instead. That is why the two .toml files in this
# kit are commented so heavily: they are the wizard's replacement, and
# nothing validates them (neither schema rejects unknown keys).
#
# Idempotent: safe to re-run. It will not overwrite an existing config
# or regenerate API keys unless you pass --force-config.

set -euo pipefail

KIT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PREFIX=/opt/firm-rag
DATA=/var/lib/firm-rag
ETC=/etc/firm-rag
SVC_USER=firmrag
DOCS_DIR=""
HOSTNAME_FQDN=""
FORCE_CONFIG=0
SKIP_ACCEPTANCE=0

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
  --user <name>       service account   (default firmrag)
  --force-config      overwrite existing configs AND regenerate API keys
  --skip-acceptance   install but do not run acceptance.sh
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --docs)     DOCS_DIR="${2:-}"; shift 2 ;;
        --hostname) HOSTNAME_FQDN="${2:-}"; shift 2 ;;
        --prefix)   PREFIX="${2:-}"; shift 2 ;;
        --data)     DATA="${2:-}"; shift 2 ;;
        --user)     SVC_USER="${2:-}"; shift 2 ;;
        --force-config)    FORCE_CONFIG=1; shift ;;
        --skip-acceptance) SKIP_ACCEPTANCE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage; die "unknown argument: $1" ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die "must run as root (systemd units, /etc, service user)"
[ -n "$DOCS_DIR" ]      || { usage; die "--docs is required"; }
[ -n "$HOSTNAME_FQDN" ] || { usage; die "--hostname is required"; }
[ -d "$DOCS_DIR" ]      || die "--docs path does not exist: $DOCS_DIR"

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

# ── 2. Directories ───────────────────────────────────────────────────
say "directories"
install -d -m 0755 "$PREFIX/bin"
install -d -m 0750 -o "$SVC_USER" -g "$SVC_USER" "$DATA"
install -d -m 0750 -o "$SVC_USER" -g "$SVC_USER" "$DATA/models"
install -d -m 0750 -o "$SVC_USER" -g "$SVC_USER" "$DATA/lib"
install -d -m 0750 -o root -g "$SVC_USER" "$ETC"

# ── 3. Binaries ──────────────────────────────────────────────────────
# Four, not three. The standard release set is sovereign-cli /
# -cli-daemon / -cli-llm; `sovereign-server` is NOT in it and is built
# specially by package.sh with --no-default-features.
#
# `svrn` is the sovereign-cli dispatcher. It resolves its siblings by
# exact filename next to its own path, so all four must land in the same
# directory and keep their names.
say "binaries → $PREFIX/bin"
for b in svrn sovereign-cli-daemon sovereign-cli-llm sovereign-server; do
    [ -f "$KIT_DIR/bin/$b" ] || die "kit is missing bin/$b"
    install -m 0755 "$KIT_DIR/bin/$b" "$PREFIX/bin/$b"
    echo "    $b"
done

# ── 4. Models ────────────────────────────────────────────────────────
say "models → $DATA/models"
for f in "$KIT_DIR"/models/*.gguf; do
    [ -e "$f" ] || die "kit contains no models/*.gguf"
    install -m 0640 -o "$SVC_USER" -g "$SVC_USER" "$f" "$DATA/models/$(basename "$f")"
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
    chown -R "$SVC_USER:$SVC_USER" "$DATA/models/paddle-ocr"
    set_dir="$DATA/models/paddle-ocr/ppocr-en-v4v5"
    for f in det.onnx rec.onnx dict.txt; do
        [ -f "$set_dir/$f" ] || die "OCR model set is incomplete: $set_dir/$f is missing.
     A partial set does not half-work — the engine refuses at ingest."
    done
    install -m 0644 -o "$SVC_USER" -g "$SVC_USER" "$KIT_DIR/ocr/libpdfium.so" "$DATA/lib/libpdfium.so" \
        || die "kit is missing ocr/libpdfium.so. Without it no PDF can be rasterized and OCR yields nothing."
    echo "    paddle-ocr + libpdfium.so"
else
    echo "    SKIPPED — no ocr/ in the kit. Scanned PDFs will be reported as"
    echo "    scanned_no_text and will not be indexed. For a litigation"
    echo "    practice this is usually the wrong tradeoff; see README.md."
fi

# ── 6. Configs ───────────────────────────────────────────────────────
say "configs → $ETC"
gen_key() { head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n'; }

write_config() {
    local src="$1" dst="$2"
    if [ -f "$dst" ] && [ "$FORCE_CONFIG" -eq 0 ]; then
        echo "    $dst exists — kept (pass --force-config to overwrite)"
        return
    fi
    sed -e "s|/var/lib/firm-rag|$DATA|g" \
        -e "s|/etc/firm-rag|$ETC|g" \
        -e "s|/opt/firm-rag|$PREFIX|g" \
        "$src" > "$dst"
    chown root:"$SVC_USER" "$dst"
    chmod 0640 "$dst"
    echo "    $(basename "$dst")"
}

write_config "$KIT_DIR/config/daemon-config.toml" "$ETC/daemon-config.toml"

if [ ! -f "$ETC/server-config.toml" ] || [ "$FORCE_CONFIG" -eq 1 ]; then
    API_KEY_1="$(gen_key)"
    write_config "$KIT_DIR/config/server-config.toml" "$ETC/server-config.toml"
    # Replace the marker INSIDE [auth.keys]. Not an append: this file
    # continues past that table, so `>>` would put the key under the
    # last section (`[iroh]`), where it parses, is silently dropped as
    # an unknown key, and leaves [auth.keys] empty — which disables auth
    # entirely with no error and no warning.
    grep -q '# __INSTALL_KEYS__' "$ETC/server-config.toml" \
        || die "server-config.toml has no '# __INSTALL_KEYS__' marker under [auth.keys].
     Refusing to guess where the API key goes: getting it wrong produces a
     config that parses cleanly and serves every route unauthenticated."
    sed -i.bak "s|# __INSTALL_KEYS__|\"$API_KEY_1\" = \"firm\"|" "$ETC/server-config.toml"
    rm -f "$ETC/server-config.toml.bak"
    # Prove it landed in the right table rather than trusting the sed:
    # everything from [auth.keys] to the next section header must contain
    # the key.
    awk '/^\[auth\.keys\]/{f=1;next} /^\[/{f=0} f' "$ETC/server-config.toml" \
        | grep -q "$API_KEY_1" \
        || die "the generated key is not inside [auth.keys] after substitution.
     Do not start the server: it would serve every route unauthenticated."
    install -m 0600 -o root -g root /dev/null "$ETC/issued-keys.txt"
    printf 'firm\t%s\n' "$API_KEY_1" >> "$ETC/issued-keys.txt"
    echo "    generated 1 API key → $ETC/issued-keys.txt (mode 0600, root only)"
    echo "    verified it landed inside [auth.keys]"
else
    echo "    $ETC/server-config.toml exists — kept, keys unchanged"
fi

if [ ! -f "$ETC/acceptance-probes.env" ]; then
    install -m 0640 -o root -g "$SVC_USER" \
        "$KIT_DIR/config/acceptance-probes.env.template" "$ETC/acceptance-probes.env"
    echo "    acceptance-probes.env (TEMPLATE — must be filled in, see below)"
fi

# ── 7. systemd ───────────────────────────────────────────────────────
say "systemd units"
for u in firm-rag-daemon firm-rag-server; do
    sed -e "s|/var/lib/firm-rag|$DATA|g" \
        -e "s|/etc/firm-rag|$ETC|g" \
        -e "s|/opt/firm-rag|$PREFIX|g" \
        -e "s|/srv/firm-docs|$DOCS_DIR|g" \
        -e "s|^User=.*|User=$SVC_USER|" \
        -e "s|^Group=.*|Group=$SVC_USER|" \
        "$KIT_DIR/systemd/$u.service" > "/etc/systemd/system/$u.service"
    echo "    $u.service"
done
systemctl daemon-reload

# ── 8. nginx ─────────────────────────────────────────────────────────
say "nginx"
install -d -m 0755 /etc/nginx/snippets
install -m 0644 "$KIT_DIR/nginx/firm-rag-proxy.conf" /etc/nginx/snippets/firm-rag-proxy.conf
sed -e "s|firm-rag\.example\.com|$HOSTNAME_FQDN|g" \
    "$KIT_DIR/nginx/firm-rag.conf" > /etc/nginx/conf.d/firm-rag.conf
echo "    /etc/nginx/conf.d/firm-rag.conf (server_name $HOSTNAME_FQDN)"
echo "    certs are NOT installed by this script — see step 4 below"

# ── 9. Start the daemon, then restore the corpus ─────────────────────
# Order matters: `corpus snapshot restore` talks to the running daemon.
say "starting daemon"
systemctl enable --now firm-rag-daemon.service

# Readiness is GET /v1/models returning 2xx. There is no dedicated ready
# command, and `systemctl start` returning does NOT mean the GGUFs are
# loaded — a 35B cold load is tens of seconds.
printf '    waiting for models to load'
ready=0
for _ in $(seq 1 120); do
    if curl -sf -o /dev/null --max-time 5 http://127.0.0.1:9741/v1/models; then ready=1; break; fi
    printf '.'; sleep 5
done
echo
[ "$ready" -eq 1 ] || die "daemon did not become ready in 10 minutes.
     journalctl -u firm-rag-daemon -n 100
     The daemon refuses to start rather than starting degraded, so the
     log names the reason. Most likely: a GGUF path in daemon-config.toml
     that does not exist (the VRAM preflight labels the slot UNREADABLE),
     or a corrupt GGUF (which dies later, at slot load)."

say "restoring the us-code corpus"
if [ -f "$KIT_DIR/corpora/us-code.tar.zst" ]; then
    sha=""
    [ -f "$KIT_DIR/corpora/us-code.sha256" ] && sha="$(cut -d' ' -f1 < "$KIT_DIR/corpora/us-code.sha256")"
    # Restore HARD-ERRORS on an embedding-dimension mismatch. That is the
    # good outcome: it means this box runs a different embed model than
    # the one that built the snapshot, and a silent restore would return
    # quietly wrong neighbours forever.
    sudo -u "$SVC_USER" "$PREFIX/bin/svrn" corpus snapshot restore \
        --archive "$KIT_DIR/corpora/us-code.tar.zst" \
        --as us-code \
        --into "$DATA/indexes" \
        ${sha:+--expected-sha256 "$sha"} \
        || die "snapshot restore failed — see the message above.
     An embedding-dimension mismatch means the embed model in
     daemon-config.toml is not the one that built this snapshot."
    echo "    us-code restored"
else
    echo "    SKIPPED — no corpora/us-code.tar.zst in the kit"
fi

# ── 10. The firm's document share ────────────────────────────────────
# `svrn corpus ingest` is NOT recursive. `corpus watch` walks the tree
# (walkdir-based) and keeps it in sync, which is what a mounted share
# needs. --ocr is what makes scanned discovery readable.
say "watching the document share: $DOCS_DIR"
sudo -u "$SVC_USER" "$PREFIX/bin/svrn" corpus watch "$DOCS_DIR" \
    --name "firm-docs" --ocr --sweep-secs 300 \
    || echo "    WARNING: could not register the share; run it by hand and see README.md"

# ── 11. Start the server ─────────────────────────────────────────────
say "starting the API server"
systemctl enable --now firm-rag-server.service
sleep 3
systemctl is-active --quiet firm-rag-server.service \
    || die "firm-rag-server did not start: journalctl -u firm-rag-server -n 50"

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

  3. Hand out the API key from $ETC/issued-keys.txt (root-readable
     only). One shared tenant: every key sees every document AND every
     conversation. That is a stated pilot limit, not a bug — README.md
     "Honest limits".

  4. Run the acceptance suite against the TLS hostname, NOT loopback:
       BASE_URL=https://$HOSTNAME_FQDN API_KEY=<key> $KIT_DIR/acceptance.sh
     Gate on the exit code: 0 ready, 1 failed, 2 could-not-judge.

EOF

if [ "$SKIP_ACCEPTANCE" -eq 0 ]; then
    say "running acceptance now (expect UNSURE until steps 1-2 are done)"
    BASE_URL="https://$HOSTNAME_FQDN" "$KIT_DIR/acceptance.sh" || true
fi
