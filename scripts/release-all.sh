#!/usr/bin/env bash
# release-all.sh — ONE command to cut the full Commonwealth AI release
# (CLI + desktop, every target) from this arm64 Mac, and optionally publish.
#
# This is the repeatable release process. It is a thin, glassbox conductor
# over the already-validated per-artifact scripts (release-cli-local.sh,
# release-desktop-local.sh → build-desktop-{macos,linux,windows}.sh); its
# job is the SEQUENCING and the GUARDS that those scripts don't own, each of
# which encodes a real failure this repo has actually hit:
#
#   • version-already-published guard — main routinely moves ahead of the
#     last release tag; reshipping a published version under the same number
#     ships different bits. We refuse it. (v0.1.20 was published while main
#     had moved on — the trap that forced the v0.2.0 bump.)
#   • disk reclaim — a stale target/debug hoarded 89GB mid-release, one
#     ENOSPC away from corrupting the podman VM. We reclaim it up front.
#   • per-leg stall watchdog — the Linux desktop leg once hung SILENTLY for
#     10.5h (qemu glslc-reap deadlock) and the wrapper still reported exit 0.
#     Every long leg now runs under a CPU-aware watchdog: no log output for
#     STALL_SECS *and* an idle VM ⇒ kill + fail loudly, never hang forever.
#   • the Linux glslc deadlock itself is fixed in build-desktop-linux.sh
#     (serial shader compile via taskset); the Windows rustup-target gap is
#     fixed in build-entrypoint-windows.sh. This driver just guards + wires.
#
# Usage:
#   scripts/release-all.sh                  # build+upload both, leave drafts
#   scripts/release-all.sh --publish        # also publish when green
#   scripts/release-all.sh --check          # run preflight only, then exit
#   scripts/release-all.sh --skip-cli
#   scripts/release-all.sh --skip-desktop
#   scripts/release-all.sh --skip-tests     # skip the workspace test gate
#   scripts/release-all.sh --no-reclaim     # don't touch target/debug
#   scripts/release-all.sh --force          # override the already-published guard
#
# Prereqs (same as the per-artifact scripts): arm64 mac, gh authed, podman
# machine up (≥16GiB), TAURI_SIGNING_PRIVATE_KEY{,_PASSWORD} exported, and
# the version bumped + tagged at HEAD (scripts/bump-desktop-version.sh, then
# git commit + git tag cli-vX.Y.Z desktop-vX.Y.Z + push). This driver does
# NOT bump or tag — releasing is a deliberate act (see bump script header).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

say()  { printf '\033[1m[release-all]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[release-all]\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31m[release-all]\033[0m %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "'$1' is required on PATH"; }

# ─── Flags ────────────────────────────────────────────────────────────
PUBLISH=0 SKIP_CLI=0 SKIP_DESKTOP=0 SKIP_TESTS=0 RECLAIM=1 CHECK_ONLY=0 FORCE=0
for a in "$@"; do
    case "$a" in
        --publish)      PUBLISH=1 ;;
        --check)        CHECK_ONLY=1 ;;
        --skip-cli)     SKIP_CLI=1 ;;
        --skip-desktop) SKIP_DESKTOP=1 ;;
        --skip-tests)   SKIP_TESTS=1 ;;
        --no-reclaim)   RECLAIM=0 ;;
        --force)        FORCE=1 ;;
        -h|--help)      sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)              err "unknown flag: $a (see --help)" ;;
    esac
done

# Tunables (env-overridable).
RELEASES_REPO="${RELEASES_REPO:-alexsbryan/svrnmesh-releases}"
STALL_SECS="${RELEASE_STALL_SECS:-1200}"       # no-output + idle-VM ⇒ hung
RECLAIM_MIN_GB="${RELEASE_RECLAIM_MIN_GB:-20}" # reclaim target/debug above this
LOG_DIR="${TMPDIR:-/tmp}/release-all.$$"
mkdir -p "$LOG_DIR"

# ─── Resolve version + tags (single source of truth: Cargo.toml) ──────
need git; need gh
VERSION="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"
[ -n "$VERSION" ] || err "cannot read [workspace.package].version from Cargo.toml"
CLI_TAG="cli-v$VERSION"
DESK_TAG="desktop-v$VERSION"
say "Release version $VERSION → tags $CLI_TAG, $DESK_TAG → repo $RELEASES_REPO"

# ─── Preflight ────────────────────────────────────────────────────────
preflight() {
    say "Preflight…"

    [[ "$(uname -sm)" == "Darwin arm64" ]] || err "this driver assumes an arm64 Mac host (got '$(uname -sm)')"
    gh auth status >/dev/null 2>&1 || err "gh is not authenticated (gh auth login)"

    # Tags must exist AND point at HEAD — releasing HEAD while a tag names a
    # different commit ships bits the tag doesn't describe.
    local head; head="$(git rev-parse HEAD)"
    local checkset=()
    (( SKIP_CLI ))     || checkset+=("$CLI_TAG")
    (( SKIP_DESKTOP )) || checkset+=("$DESK_TAG")
    for t in "${checkset[@]}"; do
        git rev-parse -q --verify "refs/tags/$t^{commit}" >/dev/null 2>&1 \
            || err "tag '$t' does not exist. Bump + tag first:
    scripts/bump-desktop-version.sh <ver> && git commit -am 'chore(release): vX.Y.Z'
    git tag $CLI_TAG $DESK_TAG && git push origin main $CLI_TAG $DESK_TAG"
        local ts; ts="$(git rev-parse "$t^{commit}")"
        [ "$ts" = "$head" ] || err "tag '$t' points at ${ts:0:12} but HEAD is ${head:0:12} — check out the tag or retag."
    done
    [ -z "$(git status --porcelain)" ] || warn "working tree is dirty — the build will include uncommitted changes."

    # Version-already-published guard: a PUBLISHED (non-draft) release at this
    # version means reshipping would overwrite shipped bits under the same
    # number. Refuse unless --force. (A draft is fine — that's our own WIP.)
    for t in "${checkset[@]}"; do
        local isdraft
        if isdraft="$(gh release view "$t" --repo "$RELEASES_REPO" --json isDraft --jq .isDraft 2>/dev/null)"; then
            if [ "$isdraft" = "false" ]; then
                (( FORCE )) && warn "$t is already PUBLISHED — --force given, proceeding (will clobber assets)." \
                            || err "$t is already PUBLISHED on $RELEASES_REPO. Bump the version, or pass --force to reship."
            fi
        fi
    done

    # Desktop needs the updater signing key or auto-update 404s for this release.
    if ! (( SKIP_DESKTOP )); then
        [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] \
            || err "TAURI_SIGNING_PRIVATE_KEY not set — desktop auto-updates need signed artifacts (normally from ~/.zshrc)."
        need podman
        local mem; mem="$(podman machine inspect --format '{{.Resources.Memory}}' 2>/dev/null || echo 0)"
        (( mem >= 16384 )) || err "podman machine has ${mem}MiB; ggml-vulkan's shader compile OOMs below ~16GiB. Resize: podman machine set --memory 24576"
        podman machine start >/dev/null 2>&1 || true
    fi

    # Disk reclaim: target/debug is dev-only; the release uses release + the
    # per-triple dirs. Reclaiming it is safe and frees the headroom the 4-leg
    # build needs (an ENOSPC mid-build corrupts the podman VM).
    if (( RECLAIM )) && [ -d target/debug ]; then
        local gb; gb="$(du -sg target/debug 2>/dev/null | cut -f1 || echo 0)"
        if (( gb >= RECLAIM_MIN_GB )); then
            if pgrep -fq "cargo|rustc"; then
                warn "target/debug is ${gb}GB but cargo/rustc is running — skipping reclaim to avoid clobbering an active build."
            else
                say "Reclaiming target/debug (${gb}GB, dev-only, unused by the release build)…"
                rm -rf target/debug
            fi
        fi
    fi
    local free; free="$(df -g "$REPO_ROOT" | awk 'NR==2{print $4}')"
    (( free >= 40 )) || warn "only ${free}GB free; a cold 4-leg desktop build wants ~40GB+."

    # Test gate — the release definition of done. sovereign-test.sh covers the
    # full cargo workspace (the same surface CI would).
    if (( SKIP_TESTS )); then
        warn "--skip-tests: NOT running the workspace test gate."
    else
        say "Running workspace test gate (scripts/sovereign-test.sh --human)…"
        ./scripts/sovereign-test.sh --human || err "test gate failed — fix before releasing (or --skip-tests to override)."
    fi

    say "Preflight OK."
}

# ─── Ensure draft releases exist on the shelf repo ────────────────────
ensure_draft() {  # ensure_draft <tag> <title>
    local tag="$1" title="$2"
    if gh release view "$tag" --repo "$RELEASES_REPO" >/dev/null 2>&1; then
        say "draft $tag already exists — appending assets"
    else
        say "creating draft release $tag"
        gh release create "$tag" --repo "$RELEASES_REPO" --draft --title "$title" \
            || gh release view "$tag" --repo "$RELEASES_REPO" >/dev/null 2>&1 \
            || err "could not create or find release $tag"
    fi
}

# ─── CPU-aware stall watchdog ─────────────────────────────────────────
# Runs "$@" writing to <log>, in its own process group. Streams nothing; on
# completion returns the child's exit code. If the log stops growing for
# STALL_SECS *and* the podman VM is idle (a real hang, not a slow single-core
# compile), it kills the process group + any of our build containers and
# returns 124. This is the backstop that turns a 10-hour silent hang into a
# 20-minute loud failure — the Linux leg's serial fix PREVENTS the known
# deadlock; this catches the unknown ones.
KILL_PATTERN='release-(cli|desktop)-local\.sh|build-desktop-(macos|linux|windows)\.sh'
run_watched() {  # run_watched <label> <log> <cmd...>
    local label="$1" log="$2"; shift 2
    say "▶ $label  (log: $log)"
    "$@" >"$log" 2>&1 &
    local pid=$!
    local last_size=-1 quiet=0 size load
    # macOS has no setsid; kill the tracked pid AND the known build-script
    # names (podman client + cargo can outlive the parent), then reap any of
    # our build containers directly.
    while kill -0 "$pid" 2>/dev/null; do
        sleep 60
        size="$(wc -c <"$log" 2>/dev/null || echo 0)"
        if [ "$size" != "$last_size" ]; then
            last_size="$size"; quiet=0; continue
        fi
        # Log quiet for 60s. Is real work happening (healthy slow compile) or
        # nothing (hung)? "Busy" must cover BOTH execution surfaces and must
        # NOT rely on aggregate CPU-idle%: the Linux leg is taskset-pinned to 1
        # of 8 VM cores, so top shows ~87% idle while fully pegged.
        #   • host legs (mac CLI/desktop): the compile runs as host `cargo`/
        #     `rustc` (matched by exact NAME — -x — so we don't false-match
        #     podman's "cargo build …" arg string).
        #   • container legs (linux/windows): host sees only `podman`; the work
        #     is in the VM, so we read the VM 1-min load average. A pegged core
        #     ⇒ load ≈ 1.0; the glslc-reap deadlock ⇒ load ≈ 0.02.
        local busy=0
        if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1 \
           || pgrep -x cargo-tauri >/dev/null 2>&1; then
            busy=1
        else
            load="$(podman machine ssh 'cat /proc/loadavg' 2>/dev/null | awk '{print $1+0}' | tail -1 || echo 1)"
            if awk "BEGIN{exit !(${load:-1} >= 0.5)}"; then busy=1; fi
        fi
        if (( busy == 0 )); then
            quiet=$(( quiet + 60 ))
            if (( quiet >= STALL_SECS )); then
                warn "STALL: '$label' produced no output for ${quiet}s with no active compile (host cargo/rustc absent, VM load=${load:-?}) — treating as hung."
                warn "  last log line: $(tail -1 "$log" 2>/dev/null | cut -c1-140)"
                kill -TERM "$pid" 2>/dev/null || true
                pkill -TERM -f "$KILL_PATTERN" 2>/dev/null || true
                sleep 5
                kill -KILL "$pid" 2>/dev/null || true
                pkill -KILL -f "$KILL_PATTERN" 2>/dev/null || true
                podman ps -q --filter ancestor=localhost/sovereign-desktop-linux-build:latest \
                             --filter ancestor=localhost/sovereign-desktop-windows-build:latest 2>/dev/null \
                    | xargs podman rm -f >/dev/null 2>&1 || true
                return 124
            fi
        else
            quiet=0   # busy on a capped core — not a hang
        fi
    done
    wait "$pid"
}

# ─── Main ─────────────────────────────────────────────────────────────
preflight
if (( CHECK_ONLY )); then say "--check: preflight passed, stopping."; exit 0; fi

(( SKIP_CLI ))     || ensure_draft "$CLI_TAG"  "svrnmesh CLI v$VERSION"
(( SKIP_DESKTOP )) || ensure_draft "$DESK_TAG" "Sovereign Desktop v$VERSION"

if ! (( SKIP_CLI )); then
    run_watched "CLI all-targets" "$LOG_DIR/cli.log" scripts/release-cli-local.sh \
        || err "CLI build/upload failed (see $LOG_DIR/cli.log)."
fi
if ! (( SKIP_DESKTOP )); then
    run_watched "Desktop all-targets" "$LOG_DIR/desktop.log" scripts/release-desktop-local.sh \
        || err "desktop build/upload failed (see $LOG_DIR/desktop.log)."
fi

# ─── Publish gate ─────────────────────────────────────────────────────
asset_count() { gh release view "$1" --repo "$RELEASES_REPO" --json assets --jq '.assets | length' 2>/dev/null || echo 0; }

if (( PUBLISH )); then
    say "Publish gate — verifying asset counts before flipping drafts public…"
    if ! (( SKIP_CLI )); then
        n_cli="$(asset_count "$CLI_TAG")"
        (( n_cli >= 4 )) || err "$CLI_TAG has only $n_cli assets (want ≥4: 3 tarballs + SHA256SUMS) — refusing to publish."
    fi
    if ! (( SKIP_DESKTOP )); then
        n_desk="$(asset_count "$DESK_TAG")"
        (( n_desk >= 12 )) || err "$DESK_TAG has only $n_desk assets (want ≥12) — refusing to publish."
    fi
    (( SKIP_CLI ))     || { say "publishing $CLI_TAG";  gh release edit "$CLI_TAG"  --repo "$RELEASES_REPO" --draft=false >/dev/null; }
    (( SKIP_DESKTOP )) || { say "publishing $DESK_TAG"; gh release edit "$DESK_TAG" --repo "$RELEASES_REPO" --draft=false >/dev/null; }
    say "Published."
else
    say "Drafts populated (not published — pass --publish, or flip manually with 'gh release edit <tag> --repo $RELEASES_REPO --draft=false')."
fi

echo
say "Done. Assets:"
(( SKIP_CLI ))     || echo "  $CLI_TAG:  $(asset_count "$CLI_TAG") assets  → https://github.com/$RELEASES_REPO/releases/tag/$CLI_TAG"
(( SKIP_DESKTOP )) || echo "  $DESK_TAG:  $(asset_count "$DESK_TAG") assets  → https://github.com/$RELEASES_REPO/releases/tag/$DESK_TAG"
