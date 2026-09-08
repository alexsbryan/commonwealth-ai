#!/usr/bin/env bash
# ei6b-local-restore — 4b's done-when, with ZERO egress.
#
# Proves the seam the seat named: there are TWO restore paths and, until
# b8541d34c, only one of them judged anything. `judge_restored_snapshot` is now
# the single decider both call, so this run drives the LOCAL path
# (`snapshot restore --archive`) — the one that previously extracted whatever
# it was handed — through all three of its verdicts on real bytes.
#
#   leg A  publish wessex-hoard locally  -> the manifest carries embed_quirks
#   leg B  restore under a DIFFERENT model label -> NameMismatch -> probe runs
#                                                -> Accepted, cosine printed
#   leg C  restore under the SAME label          -> Exact, no probe needed
#   leg D  restore an archive whose manifest's pooling is FLIPPED
#                                                -> ConfigMismatch, REFUSED,
#                                                   and nothing installed
#
# Leg D is the negative control. Without it ConfigMismatch is a gate with no
# input that can make it fire (ARCH §18.1).
#
# CONTROLS: wessex-hoard is READ (published FROM) and never written. Every
# restore lands in a cold root under test-artifacts/, never ~/.svrnmesh.
#
# Optional env: ALLOW_BUSY_BOX=1, SKIP_BUILD=1 (binaries already fresh)
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

CORPUS="${CORPUS:-wessex-hoard}"
WORK="$repo/test-artifacts/ei6b-local-restore"
CLI="$repo/target/debug/sovereign-cli"

mark() { printf '%s rc=%s %s\n' "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"; }
box()  { { date -u +%Y-%m-%dT%H:%M:%SZ; free -g | sed -n 2p; df -h /home | tail -1; } > "$out/box-$1.txt"; }
die()  { mark "$1" 1; echo "DONE rc=1" >> "$out/markers.txt"; box after 2>/dev/null || true; exit 1; }
on_signal() {
  printf 'DONE rc=%s KILLED-BY=%s %s\n' "$2" "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$out/markers.txt"
  box after 2>/dev/null || true; exit "$2"
}
trap 'on_signal SIGTERM 143' TERM
trap 'on_signal SIGINT 130'  INT
trap 'on_signal SIGHUP 129'  HUP

# ── preflight, all of it before the build ─────────────────────────────────
command -v zstd >/dev/null || die preflight-zstd
python3 -c "import zstandard, tarfile" 2>/dev/null || die preflight-python-zstandard
[[ -d "$HOME/.svrnmesh/indexes/$CORPUS" ]] || die preflight-corpus-missing
curl -sf -m 120 -X POST http://127.0.0.1:9741/v1/embeddings \
  -H 'Content-Type: application/json' -d '{"input":"preflight","model":"embed:default"}' \
  | python3 -c "import json,sys; sys.exit(0 if len(json.load(sys.stdin)['data'][0]['embedding'])==1024 else 1)" \
  || die preflight-daemon-embed
rm -rf "$WORK"; mkdir -p "$WORK"
mark preflight 0
box before

# ── LEG 0: build. `sovereign-cli` is a dispatcher that execs siblings, and it
# MUST carry --features dev-tools or it silently becomes an end-user binary.
# We invoke THIS worktree's target/debug by absolute path, so the operator's
# installed binary is never touched.
if [[ -z "${SKIP_BUILD:-}" ]]; then
  t0=$(date +%s)
  ( cd "$repo" && flock /tmp/sovereign-build.lock \
      cargo build -p sovereign-cli --features dev-tools -p sovereign-cli-llm \
        --features corpus-engine/treesitter ) > "$out/build.log" 2>&1
  rc=$?; mark build "$rc"
  printf 'build wall=%ss\n' "$(( $(date +%s) - t0 ))" >> "$out/walls.txt"
  [[ $rc -eq 0 ]] || { tail -25 "$out/build.log" >&2; die build; }
fi
[[ -x "$CLI" ]] || die binary-missing

ARCHIVE="$WORK/$CORPUS.tar.zst"

# ── LEG A: publish locally. Reads the control; writes only under test-artifacts.
"$CLI" corpus snapshot publish "$CORPUS" --output "$ARCHIVE" \
  > "$out/A-publish.log" 2>&1
rc=$?; mark A-publish "$rc"
[[ $rc -eq 0 ]] || { tail -20 "$out/A-publish.log" >&2; die A-publish; }

"$CLI" corpus snapshot inspect "$ARCHIVE" > "$out/A-inspect.log" 2>&1
mark A-inspect "$?"
# THE PUBLISHER PROOF: the manifest must now carry the embedder config.
python3 - "$ARCHIVE" > "$out/A-manifest.json" 2>"$out/A-manifest.err" <<'PY'
import io, json, sys, tarfile, zstandard as zstd
raw = zstd.ZstdDecompressor().stream_reader(open(sys.argv[1], "rb")).read()
with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as tf:
    m = next(x for x in tf.getmembers() if x.name.endswith("_snapshot_manifest.json"))
    man = json.load(tf.extractfile(m))
q = man.get("embed_quirks")
if not q:
    sys.exit("A-manifest: the published manifest declares NO embed_quirks — the publisher "
             "wiring did not land, and legs C/D below cannot mean anything")
print(json.dumps({"embedding_model": man["embedding_model"], "embed_quirks": q}, indent=2))
PY
rc=$?; mark A-manifest-declares-config "$rc"
[[ $rc -eq 0 ]] || { cat "$out/A-manifest.err" >&2; die A-manifest-declares-config; }
MODEL=$(python3 -c "import json;print(json.load(open('$out/A-manifest.json'))['embedding_model'])")
echo "published embedding_model = $MODEL" | tee -a "$out/markers-notes.txt"

restore_into() { # $1 root  $2 model label  $3 archive  $4 logname
  rm -rf "$1"; mkdir -p "$1"
  "$CLI" corpus snapshot restore --archive "$3" --into "$1" \
    --embedding-model "$2" --embedding-dim 1024 > "$out/$4.log" 2>&1
  echo $?
}

# ── LEG B: a DIFFERENT label for the same model. Config agrees, name does not
# -> NameMismatch -> the probe runs -> accepted, with the cosine printed. This
# is the arm that did not exist on this path before b8541d34c.
rc=$(restore_into "$WORK/root-b" "qwen-embedding-0.6b" "$ARCHIVE" "B-restore-namemismatch")
mark B-restore "$rc"
grep -qE 'probe cosine' "$out/B-restore-namemismatch.log" && mark B-probe-ran 0 || mark B-probe-ran 1
[[ -d "$WORK/root-b/indexes/$CORPUS" ]] && mark B-index-present 0 || mark B-index-present 1

# ── LEG C: the SAME label -> Exact, accepted without a probe.
rc=$(restore_into "$WORK/root-c" "$MODEL" "$ARCHIVE" "C-restore-exact")
mark C-restore "$rc"
[[ -d "$WORK/root-c/indexes/$CORPUS" ]] && mark C-index-present 0 || mark C-index-present 1

# ── LEG D: the NEGATIVE control. Same bytes, manifest pooling flipped.
python3 "$here/flip_pooling.py" "$ARCHIVE" "$WORK/$CORPUS-meanpooled.tar.zst" mean \
  > "$out/D-flip.log" 2>&1
rc=$?; mark D-flip "$rc"
[[ $rc -eq 0 ]] || { cat "$out/D-flip.log" >&2; die D-flip; }
rc=$(restore_into "$WORK/root-d" "$MODEL" "$WORK/$CORPUS-meanpooled.tar.zst" "D-restore-configmismatch")
mark D-restore-exit "$rc"
# The refusal must be non-zero, must NAME pooling, and must leave nothing behind.
[[ "$rc" != "0" ]] && mark D-refused 0 || mark D-refused 1
grep -qi 'pooling' "$out/D-restore-configmismatch.log" && mark D-names-pooling 0 || mark D-names-pooling 1
[[ -d "$WORK/root-d/indexes/$CORPUS" ]] && mark D-nothing-installed 1 || mark D-nothing-installed 0

# ── the verdict ────────────────────────────────────────────────────────────
ok=1
for m in A-publish:0 A-manifest-declares-config:0 B-restore:0 B-probe-ran:0 B-index-present:0 \
         C-restore:0 C-index-present:0 D-refused:0 D-names-pooling:0 D-nothing-installed:0; do
  k="${m%%:*}"; want="${m##*:}"
  got=$(grep -m1 "^$k rc=" "$out/markers.txt" | sed 's/.*rc=\([0-9]*\).*/\1/')
  [[ "$got" == "$want" ]] || { echo "FAILED $k rc=$got want=$want"; ok=0; }
done
if [[ $ok == 1 ]]; then
  mark VERDICT-LOCAL-RESTORE-JUDGED 0
  echo "VERDICT-LOCAL-RESTORE-JUDGED — the local path accepts by probe, accepts by name, and"
  echo "  refuses a flipped-pooling archive by name without installing it."
else
  mark VERDICT-LOCAL-RESTORE-INCOMPLETE 1
fi
grep -hE 'probe cosine|REFUSED|COULD-NOT-JUDGE|Restored' "$out"/[BCD]-*.log > "$out/SENTENCES.txt" 2>/dev/null
box after
echo "DONE rc=0" >> "$out/markers.txt"
echo "artifacts: $out"
