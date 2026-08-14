#!/usr/bin/env bash
# extract-evidence.sh — the compounding act, made mechanical.
#
# Run 2's flight recorder keeps one evidence-window-{round}.json per round.
# Each chunk (WindowChunk, ICD) is extracted into a markdown file whose
# header block carries the chunk's identity (locator, source_url, custody,
# provenance_class) so the ingested corpus keeps the provenance visible:
#
#   research/deep-research/demo/extract-evidence.sh <run_dir> <out_folder>
#   svrn corpus ingest <out_folder> --corpus apollo11-evidence
#   svrn deep-research "<same question>" --run-dir <new_dir> \
#     --corpora apollo11-evidence
#
# The corpus id is the folder basename (apollo11-evidence), which is why
# the ingest step needs no explicit --corpus.
set -euo pipefail

RUN_DIR="${1:?usage: extract-evidence.sh <run_dir> <out_folder>}"
OUT="${2:?usage: extract-evidence.sh <run_dir> <out_folder>}"
mkdir -p "$OUT"

round=0
for window in "$RUN_DIR"/evidence-window-*.json; do
  [ -e "$window" ] || continue
  round=$((round + 1))
  python3 - "$window" "$OUT" "$round" << 'PY'
import json, pathlib, sys
window_path, out, round_no = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
data = json.loads(window_path.read_text())
chunks = data.get("chunks", [])
written = 0
for chunk in chunks:
    cid = chunk.get("id") or f"round-{round_no}-{written}"
    safe = "".join(c if c.isalnum() or c in "-_." else "-" for c in cid).strip("-") or f"chunk-{written}"
    header = [f"# {chunk.get('locator', cid)}"]
    for key in ("source_url", "custody", "provenance_class"):
        if chunk.get(key):
            header.append(f"- {key}: {chunk[key]}")
    if chunk.get("tags"):
        header.append(f"- tags: {', '.join(chunk['tags'])}")
    if chunk.get("ingested_into"):
        header.append(f"- ingested_into: {chunk['ingested_into']}")
    body = chunk.get("content", "").strip()
    (out / f"{safe}.md").write_text("\n".join(header) + "\n\n" + body + "\n")
    written += 1
print(f"window round {round_no}: {written} chunks -> {out}")
PY
done

if [ "$round" -eq 0 ]; then
  echo "no evidence-window-*.json in $RUN_DIR — nothing to extract" >&2
  exit 1
fi
echo "done: $OUT is ready for: svrn corpus ingest $OUT --corpus $(basename "$OUT")"
