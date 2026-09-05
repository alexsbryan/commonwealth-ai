#!/usr/bin/env bash
# ei-6-distribution acceptance — corpus-mcp/acceptance.sh against a bare
# llama-server, TWICE, with the cold-root pull leg opted in on the second.
#
# TWO LEGS RATHER THAN ONE INVOCATION. `ACCEPT_PULL=1` would run everything
# leg 1 runs and the pull as well, so one invocation looks cheaper. It is not:
# acceptance.sh `fail()`s on the first bad assertion, so a pull that dies on
# the network (~875 MB from HuggingFace, the one leg whose failure mode is
# somebody else's uptime) would take the llama-server verdict down with it.
# The done-when's measured arm is leg 1; the pull is leg 2 and can fail alone.
#
# Staged for the run channel because the two legs together exceed the harness's
# 10-minute per-call ceiling. The seat launches it.
#
# Env the CALLER sets — an absent one is refused, never guessed (ARCH §18.3):
#   EMBED_GGUF   the embedding .gguf (Qwen3-Embedding-0.6B-Q8_0.gguf, dim 1024)
# Optional:
#   PULL_CORPUS  corpus the cold-root leg installs (default: acceptance.sh's CORPUS, `sep`)
#   SKIP_PULL=1  run leg 1 only (leg 2 then reports NEVER-RAN by name, not absent)
#   ALLOW_BUSY_BOX=1  deliberate override of the box floors below
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

mark() { printf '%s rc=%s %s\n' "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"; }
die() { mark "$1" 1; echo "DONE rc=1" >> "$out/markers.txt"; exit 1; }
box() { { date -u +%Y-%m-%dT%H:%M:%SZ; free -g | sed -n 2p; df -h /home | tail -1;
          echo "builds: $(pgrep -af 'cargo|rustc' | grep -v lspmux | grep -vc pgrep)"; } > "$out/box-$1.txt"; }

# ── preflight: every external dependency a LATER leg needs, checked FIRST ────
#
# The rule this encodes was paid for on 2026-09-05: an ei-5b run spent 5,418 s
# on an ingest and then died in its scorer on a missing `jq`. A dependency
# check that runs after the expensive step is not a preflight, it is an
# autopsy. acceptance.sh carries the same loop for its own legs; this one
# guards the wrapper's, and both are cheap.
: "${EMBED_GGUF:?run.sh: EMBED_GGUF is required — name the embedding model .gguf}"
[[ -f "$EMBED_GGUF" ]] || die preflight-embed-gguf
for tool in jq python3 curl llama-server; do
  command -v "$tool" >/dev/null || { echo "run.sh: $tool not on PATH" >&2; die "preflight-tool-$tool"; }
done
mark preflight-tools 0

# The binary must exist AND be newer than the sources it claims to be.
# Measuring a stale build is this repo's recorded way of validating old code
# (AGENTS.md: verify what actually RUNS, never assume the last build was
# yours), and acceptance.sh's own check is only `-x`.
bin="$repo/target/debug/corpus-mcp"
[[ -x "$bin" ]] || { echo "run.sh: $bin not built" >&2; die preflight-binary; }
newest_src="$(find "$repo/corpus-mcp/src" "$repo/corpus-mcp/Cargo.toml" -newer "$bin" 2>/dev/null | head -1)"
if [[ -n "$newest_src" ]]; then
  echo "run.sh: REFUSED — $newest_src is newer than $bin. This would measure a stale binary." >&2
  die preflight-binary-stale
fi
mark preflight-binary 0

# ── the box floors: RECORDED and ENFORCED (ARCH §18.1) ──────────────────────
MEM_FLOOR_GB="${MEM_FLOOR_GB:-20}"
DISK_FLOOR_GB="${DISK_FLOOR_GB:-120}"
box before
builds_now=$(pgrep -af 'cargo|rustc' | grep -v lspmux | grep -vc pgrep)
avail_now=$(awk '/MemAvailable/{print int($2/1048576)}' /proc/meminfo)
disk_now=$(df --output=avail -BG /home | tail -1 | tr -dc '0-9')
if [[ -z "${ALLOW_BUSY_BOX:-}" ]] && { (( builds_now != 0 )) || (( avail_now < MEM_FLOOR_GB )) || (( disk_now < DISK_FLOOR_GB )); }; then
  echo "run.sh: REFUSED — builds=$builds_now (want 0), MemAvailable=${avail_now}G (want >= ${MEM_FLOOR_GB}G)," \
       "disk=${disk_now}G (want >= ${DISK_FLOOR_GB}G; leg 2 pulls ~875 MB and extracts it)." \
       "Override with ALLOW_BUSY_BOX=1 if you mean it." >&2
  die box-before
fi
mark box-before 0

# The cold root leg 2 needs. Refused if it already exists: a warm root would
# make the leg pass WITHOUT pulling, which is the one thing it exists to prove.
PULL_ROOT="$repo/test-artifacts/ei6-pull-root"
if [[ -z "${SKIP_PULL:-}" && -e "$PULL_ROOT" ]]; then
  echo "run.sh: REFUSED — $PULL_ROOT exists; leg 2 needs a COLD root. Remove it." >&2
  die preflight-cold-root
fi
mark preflight-cold-root 0

cd "$repo"
worst=0

# ── leg 1: the measured llama-server arm (no opt-in legs) ───────────────────
t0=$(date +%s)
EMBED_GGUF="$EMBED_GGUF" "$repo/corpus-mcp/acceptance.sh" > "$out/leg1-llama-server.log" 2>&1
rc1=$?; mark leg1-llama-server "$rc1"
printf 'leg1 wall=%ss\n' "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"
(( rc1 > worst )) && worst=$rc1

# Per-leg outcomes read back out of the script's OWN assertion lines rather
# than re-derived here — one decider for what each leg said (ARCH §10.6).
for leg in \
  'recipe new -> wrote my-coins.toml:recipe-new-writes' \
  'recipe new -> refuses to overwrite:recipe-new-refuses' \
  'discovery -> all three rungs probed and named:discovery-names-rungs' \
  'discovery -> a named --base-url is refused:discovery-no-substitution' \
  'ollama arm ->:ollama-verdict-stated' \
  'frontend up on:embed-server' \
  'ask\(.*\) ->:ask' \
  'cargo tree -p corpus-mcp:dep-tree' \
  'acceptance: PASS:overall-pass' ; do
  pat="${leg%%:*}"; name="${leg##*:}"
  if grep -qE "$pat" "$out/leg1-llama-server.log"; then mark "leg1-$name" 0; else mark "leg1-$name" 1; fi
done
# Which way the Ollama arm went, recorded as its own marker: COULD-NOT-RUN is
# a verdict this campaign accepts by name, and it must be distinguishable from
# a leg that never printed anything (ARCH §18.2, four verdicts not two).
if   grep -q 'ollama arm -> PASS'          "$out/leg1-llama-server.log"; then mark leg1-ollama-arm-PASS 0
elif grep -q 'ollama arm -> COULD-NOT-RUN' "$out/leg1-llama-server.log"; then mark leg1-ollama-arm-COULD-NOT-RUN 0
else mark leg1-ollama-arm-SILENT 1; fi

# ── leg 2: the cold-root pull (~875 MB of egress) ───────────────────────────
if [[ -n "${SKIP_PULL:-}" ]]; then
  mark leg2-pull-NEVER-RAN 0
  echo "leg 2 (cold-root pull): NEVER-RAN — SKIP_PULL was set" >> "$out/verdicts.txt"
else
  t0=$(date +%s)
  ACCEPT_PULL=1 PULL_ROOT="$PULL_ROOT" ${PULL_CORPUS:+PULL_CORPUS="$PULL_CORPUS"} \
    EMBED_GGUF="$EMBED_GGUF" "$repo/corpus-mcp/acceptance.sh" > "$out/leg2-cold-pull.log" 2>&1
  rc2=$?; mark leg2-cold-pull "$rc2"
  printf 'leg2 wall=%ss\n' "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"
  (( rc2 > worst )) && worst=$rc2
  for leg in \
    'pull-if-absent -> cold root:pull-started' \
    'is not installed — pulling the prebuilt snapshot:pull-was-real' \
    'pulled onto a cold root and SERVED a cited answer:pull-served' ; do
    pat="${leg%%:*}"; name="${leg##*:}"
    if grep -qE "$pat" "$out/leg2-cold-pull.log"; then mark "leg2-$name" 0; else mark "leg2-$name" 1; fi
  done
  # The leg removes its own root on success; if it died mid-pull the root is
  # left for triage and its size is recorded rather than silently deleted.
  if [[ -e "$PULL_ROOT" ]]; then du -sh "$PULL_ROOT" >> "$out/leftover-root.txt" 2>&1; fi
fi

grep -hE 'acceptance: (PASS|FAIL)|COULD-NOT-JUDGE|COULD-NOT-RUN|NEVER-RAN|-> (PASS|ok)|recall' \
  "$out"/leg*.log >> "$out/verdicts.txt" 2>/dev/null
box after
echo "DONE rc=$worst" >> "$out/markers.txt"
exit "$worst"
