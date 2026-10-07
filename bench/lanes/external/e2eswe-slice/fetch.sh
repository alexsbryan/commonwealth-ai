#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Fetch the pinned E2E-SWE upstream tree and materialize the four battery tasks
# into target/e2eswe-slice/. The task CONTENT is never committed (CC BY-NC 4.0,
# benchmarking-only): this script is the only path it enters the tree through,
# and every byte it lays down is checked against the committed manifest.json.
# Conventions follow bench/lanes/agent-coding/arms/build-llama-server.sh:
# pinned commit, codeload tarball, idempotent, provenance record with sha256.
#
#   ./fetch.sh            # from this directory
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${E2ESWE_TARGET:-$HERE/../../../../target/e2eswe-slice}"
COMMIT="$(python3 -c "import json;print(json.load(open('$HERE/manifest.json'))['upstream_commit'])")"
TARBALL_SHA="$(python3 -c "import json;print(json.load(open('$HERE/manifest.json'))['tarball_sha256'])")"
SRC="$OUT/upstream/E2E-SWE-$COMMIT"

# Idempotent: a verified tree is left alone.
if [ -f "$OUT/tasks/ldaptor/instruction.md" ] && [ -f "$SRC/.verified" ]; then
  echo "fetch: already materialized at $OUT/tasks (commit $COMMIT)"
  exit 0
fi

mkdir -p "$OUT/upstream"
tarball="$OUT/upstream/E2E-SWE-$COMMIT.tar.gz"
if [ ! -f "$tarball" ]; then
  echo "fetch: downloading E2E-SWE @ $COMMIT"
  curl -fsSL "https://codeload.github.com/facebookresearch/E2E-SWE/tar.gz/$COMMIT" -o "$tarball"
fi
echo "$TARBALL_SHA  $tarball" | sha256sum -c - > "$OUT/upstream/source.txt" 2>/dev/null || {
  # codeload tarballs are regenerated per download and their gzip bytes are
  # not stable across requests, so a mismatch here is expected and fine: the
  # content pin is the per-file manifest, verified below. The tarball digest
  # is recorded as provenance either way.
  echo "fetch: tarball digest differs from the recorded one (expected; content pin is the manifest)" >&2
  {
    echo "tarball: $tarball"
    echo "sha256(actual): $(sha256sum "$tarball" | cut -d' ' -f1)"
    echo "sha256(recorded-at-manifest-time): $TARBALL_SHA"
    echo "commit: $COMMIT"
  } > "$OUT/upstream/source.txt"
}
echo "commit: $COMMIT" >> "$OUT/upstream/source.txt"
echo "tarball: $tarball" >> "$OUT/upstream/source.txt"

rm -rf "$SRC"
tar xzf "$tarball" -C "$OUT/upstream"
touch "$SRC/.verified"

# Materialize the four tasks, then prove every file against the manifest.
python3 - "$HERE" "$OUT" <<'PY'
import hashlib, json, pathlib, shutil, sys
here, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
man = json.loads((here / "manifest.json").read_text())
src = out / "upstream" / f"E2E-SWE-{man['upstream_commit']}"
for tid in ("ldaptor", "cement", "j1939", "hojichar"):
    dest = out / "tasks" / tid
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(src / "tasks" / tid, dest, ignore=shutil.ignore_patterns("solution"))
seen = {}
for p in sorted(out.joinpath("tasks").rglob("*")):
    if p.is_file():
        rel = f"tasks/{p.relative_to(out / 'tasks')}"
        seen[rel] = hashlib.sha256(p.read_bytes()).hexdigest()
missing = sorted(set(man["files"]) - set(seen) - {"agents/e2e_swe_mini_sweagent.yaml"})
extra = sorted(set(seen) - set(man["files"]))
bad = sorted(k for k in seen if k in man["files"] and seen[k] != man["files"][k])
yaml_rel = "agents/e2e_swe_mini_sweagent.yaml"
yaml_ok = hashlib.sha256((src / yaml_rel).read_bytes()).hexdigest() == man["files"][yaml_rel]
if missing or extra or bad or not yaml_ok:
    for k in missing: print(f"MANIFEST: missing {k}", file=sys.stderr)
    for k in extra: print(f"MANIFEST: unexpected {k}", file=sys.stderr)
    for k in bad: print(f"MANIFEST: hash mismatch {k}", file=sys.stderr)
    if not yaml_ok: print("MANIFEST: agent yaml hash mismatch", file=sys.stderr)
    raise SystemExit("fetch: materialized tree does not match manifest.json")
print(f"fetch: {len(seen)} task files verified against manifest ({len(missing)} missing, {len(bad)} bad)")
PY

echo "fetch: ok — tasks at $OUT/tasks, agent config at $SRC/agents/e2e_swe_mini_sweagent.yaml"
