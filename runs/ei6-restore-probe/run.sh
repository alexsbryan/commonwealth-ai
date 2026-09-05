#!/usr/bin/env bash
# ei-6 restore probe — does `corpus serve --corpus sep` on a cold root RESTORE
# the snapshot, or discard it and rebuild from source?
#
# WHY THIS EXISTS. Run 20260905T181423Z spent 93 minutes and was OOM-killed at
# a 14G cap: the snapshot's embedding-space probe rejected it, `ingest`
# discarded the extracted index and started a full rebuild of ~182k paragraphs.
# The mechanism was reconstructed from file mtimes because the serve's stderr
# went to a trap-deleted mktemp. Both of those are now fixed, and this run is
# the cheap decisive experiment the seat approved (directive 2026-09-05 13:0x):
# reach the restore DECISION, ~8 min, and read what it said.
#
# WHAT STOPS IT, AND WHY THAT IS THE POINT. `PULL_DEADLINE_MINS=12` makes
# corpus-mcp's OWN refusal the thing that ends a fall-through — not the unit's
# cap. A restore is ~6 min (measured), so 12 allows it and refuses a rebuild.
# If the run ends by cgroup OOM or RuntimeMaxSec instead, the fix did not hold
# and THAT is the verdict — do not read a kill as a pass.
#
# Env the CALLER sets:
#   EMBED_GGUF   the embedding .gguf (absolute; not present in this worktree)
# Optional: PULL_DEADLINE_MINS (default 12), ALLOW_BUSY_BOX=1
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

mark() { printf '%s rc=%s %s\n' "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"; }
box() { { date -u +%Y-%m-%dT%H:%M:%SZ; free -g | sed -n 2p; df -h /home | tail -1; } > "$out/box-$1.txt"; }
die() { mark "$1" 1; echo "DONE rc=1" >> "$out/markers.txt"; exit 1; }

# The terminal marker on every exit path, including a kill — the gap that made
# the last run unreadable (it wrote no DONE at all, so "killed" looked exactly
# like "still running").
on_signal() {
  printf 'DONE rc=%s KILLED-BY=%s %s\n' "$2" "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"
  box after 2>/dev/null || true
  cp "$repo/test-artifacts/ei6-probe-root.pull.err" "$out/" 2>/dev/null || true
  exit "$2"
}
trap 'on_signal SIGTERM 143' TERM
trap 'on_signal SIGINT 130'  INT
trap 'on_signal SIGHUP 129'  HUP

: "${EMBED_GGUF:?run.sh: EMBED_GGUF is required (absolute path)}"
[[ -f "$EMBED_GGUF" ]] || die preflight-embed-gguf
for tool in jq python3 curl llama-server; do
  command -v "$tool" >/dev/null || die "preflight-tool-$tool"
done
bin="$repo/target/debug/corpus-mcp"
[[ -x "$bin" ]] || die preflight-binary
[[ -z "$(find "$repo/corpus-mcp/src" "$repo/corpus-mcp/Cargo.toml" -newer "$bin" 2>/dev/null)" ]] \
  || die preflight-binary-stale
mark preflight 0

PROBE_ROOT="$repo/test-artifacts/ei6-probe-root"
[[ -e "$PROBE_ROOT" ]] && die preflight-cold-root
box before
avail=$(awk '/MemAvailable/{print int($2/1048576)}' /proc/meminfo)
disk=$(df --output=avail -BG /home | tail -1 | tr -dc '0-9')
if [[ -z "${ALLOW_BUSY_BOX:-}" ]] && { (( avail < 20 )) || (( disk < 120 )); }; then
  echo "run.sh: REFUSED — MemAvailable=${avail}G (want >=20), disk=${disk}G (want >=120)" >&2
  die box-before
fi
mark box-before 0

cd "$repo"
t0=$(date +%s)
(
  export ACCEPT_PULL=1 EMBED_GGUF="$EMBED_GGUF"
  export PULL_ROOT="$PROBE_ROOT" PULL_CORPUS=sep
  export PULL_DEADLINE_MINS="${PULL_DEADLINE_MINS:-12}"
  exec "$repo/corpus-mcp/acceptance.sh"
) > "$out/probe.log" 2>&1
rc=$?
mark probe "$rc"
printf 'probe wall=%ss\n' "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"

# The serve's own stderr is the artifact this whole run exists to capture.
cp "$PROBE_ROOT.pull.err" "$out/" 2>/dev/null && mark pull-err-captured 0 || mark pull-err-captured 1

# WHICH OUTCOME, named. These are the four the experiment can produce, and
# each is a different verdict — none of them is "it failed" (ARCH §18.2).
err="$out/ei6-probe-root.pull.err"
if   grep -q 'pulled onto a cold root and SERVED' "$out/probe.log" 2>/dev/null; then
  mark VERDICT-RESTORED-AND-SERVED 0
elif grep -qE 'the pull has run [0-9]+ minutes' "$err" 2>/dev/null; then
  mark VERDICT-REFUSED-BY-DEADLINE 0
elif grep -qE 'probe FAILED|probe could not run|falling through to full ingest' "$err" 2>/dev/null; then
  mark VERDICT-FELL-THROUGH-CAUGHT-BY-ASSERTION 0
elif grep -q 'embedding model name not configured' "$err" 2>/dev/null; then
  # Added after run 20260905T201154Z, which hit exactly this and could only be
  # reported UNCLASSIFIED. A precondition error before any egress is its own
  # outcome and a cheap one — the run cost 4 seconds and zero bytes.
  mark VERDICT-PRECONDITION-ERROR-NO-EGRESS 1
else
  mark VERDICT-UNCLASSIFIED 1
fi

# Leave the root for triage; it is the evidence, and 1.8 GB is affordable.
du -sh "$PROBE_ROOT" > "$out/root-size.txt" 2>&1 || true
ls "$PROBE_ROOT/indexes" 2>/dev/null | head -5 > "$out/root-indexes-head.txt" || true
[[ -d "$PROBE_ROOT/indexes/sep" ]] && mark indexes-sep-present 0 || mark indexes-sep-present 1
box after
echo "DONE rc=$rc" >> "$out/markers.txt"
exit "$rc"
