#!/usr/bin/env bash
# run-arms.sh — the T1c phase-2 measurement arms (order deep-research-t1c,
# pre-registered in research/deep-research/adversarial/pre-registration.md).
#
#   arm 1 — the LOOP: 13 flights (12 v0 estate decks + the v1 source
#           deck) through the shipped CLI on the mock-deck surface.
#   arm 2 — the ONE-SHOT: the same 13 questions through the comparator
#           (sovereign-core/tests/oneshot_rag.rs — production Deck +
#           MockBackendImpl + synthesize::draft_round; ONLY the loop
#           differs).
#
# Questions are EXTRACTED from the frozen bank files (bank/seeds.md +
# bank/v1/seeds.md) — the driver never hardcodes a question, so the
# pairs can never drift from the mint. Writes:
#   arms/runs/pairs.json          — the (id, deck, question) triples
#   arms/runs/loop/<id>/dr-*/     — flight recorders (the CLI run dirs)
#   arms/runs/loop/<id>.console.log
#   arms/runs/oneshot/oneshot-<id>.md + -window.json
#
# The daemon must be up (the pre-registered model pin). No live web:
# search/fetch are served from the decks.
#
# The acquisition budget is 12/12 by default (order deep-research-t1d
# re-measurement — pre-registered: the t1c run exhausted the v1
# round-1 budget at 4/4 before the breadth fix's frontier could be
# asked). Set SEARCH_ALLOWANCE / FETCH_ALLOWANCE to reproduce the t1c
# 4/4 protocol verbatim.
set -u

DR_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARMS="$DR_ROOT/arms"
# Run-root override (order deep-research-t4a, pre-registered): the t4a
# battery re-flights the frozen bank under the amended gate and MUST
# write to a fresh root (ARMS_RUN_ROOT="$ARMS/runs-t4a") so the frozen
# arms/runs/ from t1c/t2c/t1h is never touched. Default = the
# historical root (verbatim pre-t4a behavior).
RUNS="${ARMS_RUN_ROOT:-$ARMS/runs}"
LOOP="$RUNS/loop"
ONESHOT="$RUNS/oneshot"
BIN="${SOVEREIGN_BIN:-sovereign}"
TOOLBOX="${SOVEREIGN_TOOLBOX:-sovereign-vulkan}"
SEARCH_ALLOWANCE="${SEARCH_ALLOWANCE:-12}"
FETCH_ALLOWANCE="${FETCH_ALLOWANCE:-12}"

mkdir -p "$LOOP" "$ONESHOT"

# --- 1. extract the 13 questions from the frozen bank ----------------
python3 - "$DR_ROOT" "$RUNS/pairs.json" <<'PY'
import json, pathlib, re, sys
root = pathlib.Path(sys.argv[1])
out = pathlib.Path(sys.argv[2])

v0_text = (root / "bank/seeds.md").read_text()
v0_questions = re.findall(r'\*\*Question:\*\* "((?:[^"]|\\")*)"', v0_text, re.S)
v0_questions = [" ".join(q.split()) for q in v0_questions]
assert len(v0_questions) == 12, f"expected 12 v0 questions, found {len(v0_questions)}"

v1_text = (root / "bank/v1/seeds.md").read_text()
v1_m = re.search(r'## The question\s*\n+"((?:[^"]|\\")*)"', v1_text, re.S)
assert v1_m, "v1 question not found under ## The question"
v1_question = " ".join(v1_m.group(1).split())

pairs = [{"id": f"seed-{i+1:02d}", "deck": str(root / f"arms/decks/seed-{i+1:02d}"),
          "question": q} for i, q in enumerate(v0_questions)]
pairs.append({"id": "v1", "deck": str(root / "bank/v1/deck"), "question": v1_question})

for p in pairs:
    assert pathlib.Path(p["deck"]).is_dir(), f"deck dir missing: {p['deck']}"
    assert p["question"], f"empty question for {p['id']}"

out.write_text(json.dumps(pairs, indent=2))
print(f"pairs: {len(pairs)} ({len(v0_questions)} v0 + 1 v1) -> {out}")
PY

# --- 2. the LOOP arm — 13 flights through the shipped CLI ------------
echo "=== loop arm: 13 flights ==="
python3 -c "
import json
pairs = json.load(open('$RUNS/pairs.json'))
for p in pairs:
    print(f\"{p['id']}\t{p['question']}\")
"
while IFS=$'\t' read -r id question; do
    deck="$ARMS/decks/$id"
    [ "$id" = "v1" ] && deck="$DR_ROOT/bank/v1/deck"
    echo "=== loop: $id ==="
    "$BIN" deep-research "$question" \
        --backend mock --mock-deck "$deck" \
        --run-dir "$LOOP/$id" --max-rounds 3 \
        --search "$SEARCH_ALLOWANCE" --fetch "$FETCH_ALLOWANCE" \
        > "$LOOP/$id.console.log" 2>&1
    echo "exit=$? (see $LOOP/$id.console.log)"
done < <(python3 -c "
import json
pairs = json.load(open('$RUNS/pairs.json'))
for p in pairs:
    print(f\"{p['id']}\t{p['question']}\")
")

# --- 3. the ONE-SHOT arm — the comparator test -----------------------
echo "=== one-shot arm: the comparator (production Deck + draft_round) ==="
toolbox run -c "$TOOLBOX" env \
    DR_ARM_PAIRS="$RUNS/pairs.json" \
    DR_ARM_OUT="$ONESHOT" \
    cargo test --test oneshot_rag -- --ignored
echo "one-shot exit=$?"
echo "=== arms complete: pairs.json + loop/ + oneshot/ under $RUNS ==="
