#!/usr/bin/env bash
# The enrichment-as-plugin acceptance test — the criterion the whole plan is
# for (operator, 2026-09-03):
#
#   Someone should be able to run a query against a corpus-engine MCP with our
#   SEP corpus, using a plain `llama-server` as their inference frontend — and
#   ideally with the enrichments.
#
# Run on a machine with NO sovereign daemon required:
#   1. a bare inference frontend, nothing of ours in it (llama-server);
#   2. corpus-mcp pointed at it, over stdio;
#   3. a cited corpus_search and a tier-1.5 atoms_lookup, asserted on;
#   3b. `ask` — the composed default (EPISTEMIC_INDEX.md §4): cited passages
#      plus the map of ideas the atlas walk traversed to reach them. The
#      MECHANISM is asserted on every installed fixture; the >=3-themes bar
#      is asserted where the atlas on disk can carry it and reported
#      COULD-NOT-JUDGE, with the count, where it cannot (ei-4-walk done-when,
#      §6 row 3);
#   3c. `corpus ingest <recipe.toml>` — the WRITE half, and the whole of
#      EPISTEMIC_INDEX.md §4: a recipe, two bare llama-server processes
#      (chat + embed), acquire -> chunk -> embed -> index -> the atlas
#      enrichment, with no daemon anywhere. Scored against
#      sovereign-recipes/wessex-hoard/truth.json by the recall table in
#      scripts/setup-numismatics-corpus.sh, beside the daemon-built
#      wessex-hoard control, and then asked a question through `ask`.
#      This leg is the SLOW one (a live model over ~20 chapters), so it is
#      opt-in via ACCEPT_INGEST=1 and reports NEVER-RAN by name otherwise
#      (ARCH §18.2) rather than being silently absent.
#   4. the dep tree, asserted free of llama.cpp / ort / iroh.
#
# Env: EMBED_GGUF (default sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf),
#      PORT (8089), CORPUS (sep), ATLAS_CORPUS (sep-freewill),
#      ONTOLOGY_CORPUS (wessex-hoard — a corpus that DECLARED types; SEP
#      declares none, so the ontology read is judged on this one and reported
#      COULD-NOT-JUDGE when it is not installed), ASK_CORPORA (the corpora
#      the `ask` step runs on; default `wessex-hoard
#      brothers-karamazov-book-1`), CORPUS_MCP (target/debug/corpus-mcp),
#      QUERY, THEMATIC_QUERY.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
cd "$repo"

EMBED_GGUF="${EMBED_GGUF:-sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf}"
PORT="${PORT:-8089}"
CORPUS="${CORPUS:-sep}"
ATLAS_CORPUS="${ATLAS_CORPUS:-sep-freewill}"
ONTOLOGY_CORPUS="${ONTOLOGY_CORPUS:-wessex-hoard}"
DATA_ROOT="${SOVEREIGN_DATA_DIR:-$HOME/.svrnmesh}"
CORPUS_MCP="${CORPUS_MCP:-target/debug/corpus-mcp}"
# Every corpus the `ask` step runs on; each one not installed here is
# reported COULD-NOT-JUDGE rather than skipped silently. wessex-hoard is
# §6 row 3's own fixture (declared types, an ontology.json, near-full seed
# coverage); brothers-karamazov-book-1 is the literary half the EI1 lane
# measures.
ASK_CORPORA="${ASK_CORPORA:-wessex-hoard brothers-karamazov-book-1}"
QUERY="${QUERY:-van Inwagen consequence argument}"
# ── step 3c: the ingest leg ──
# A NEW corpus id, always. The daemon-built `wessex-hoard` on this host is the
# CONTROL this run is scored against; writing into it would destroy the only
# thing the number means.
ACCEPT_INGEST="${ACCEPT_INGEST:-}"
CHAT_GGUF="${CHAT_GGUF:-}"
CHAT_PORT="${CHAT_PORT:-8090}"
INGEST_CORPUS="${INGEST_CORPUS:-wessex-hoard-bare}"
# A FORECAST mode, not a shortcut. Set INGEST_CHAPTERS to a comma-separated
# list of manifest section ids (the verb's own `--chapters` shape) to ingest only those, so the per-chapter wall
# can be measured before a full run is committed to a lane window. A partial
# atlas cannot be compared to the control on truth.json recall — fewer
# chapters means fewer atoms for reasons that have nothing to do with the
# endpoint — so this mode reports the recall leg COULD-NOT-JUDGE by name
# (ARCH §18.2/§18.3) rather than printing a number that would read as the bar.
INGEST_CHAPTERS="${INGEST_CHAPTERS:-}"
FIXTURE_DIR="$repo/sovereign-recipes/wessex-hoard"
# A THEMATIC question, deliberately: it is the row `ask` must classify onto
# (Configuration + concept Entity seeds, Involves -> Tension -> Grounds), and
# it is the kind the EI1 lane measures. Phrased the way a reader asks, not
# the way the map's exemplars are written — a question that echoes an
# exemplar back would be testing the centroid against itself (ARCH §18.1).
THEMATIC_QUERY="${THEMATIC_QUERY:-What themes does this book establish, and what grounds each one?}"
work="$(mktemp -d)"
trap 'kill ${server_pid:-} ${chat_pid:-} 2>/dev/null || true; rm -rf "$work"' EXIT

fail() { echo "acceptance: FAIL — $*" >&2; exit 1; }
[[ -x "$CORPUS_MCP" ]] || fail "$CORPUS_MCP not built (cargo build -p corpus-mcp)"
[[ -f "$EMBED_GGUF" ]] || fail "embedding model $EMBED_GGUF not found"
command -v llama-server >/dev/null || fail "llama-server not on PATH"

# ── 1. bare frontend ────────────────────────────────────────────────────────
llama-server -m "$EMBED_GGUF" --embeddings --host 127.0.0.1 --port "$PORT" \
  >"$work/llama-server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 120); do
  curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  kill -0 "$server_pid" 2>/dev/null || { cat "$work/llama-server.log" >&2; fail "llama-server exited"; }
  sleep 1
done
curl -sf "http://127.0.0.1:$PORT/health" >/dev/null || fail "llama-server never became healthy"
cap_code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/oicp/v1/capabilities")"
echo "acceptance: frontend up on :$PORT; /oicp/v1/capabilities -> $cap_code (404 = the baseline path under test)"

# ── 2 + 3. the host, driven over stdio ──────────────────────────────────────
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"acceptance","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"corpus_search","arguments":{"query":"%s","corpus":"%s"}}}\n' "$QUERY" "$CORPUS"
  printf '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"atoms_lookup","arguments":{"corpus":"%s","kind":"claim","limit":5}}}\n' "$ATLAS_CORPUS"
  printf '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"corpus_ontology","arguments":{"corpus":"%s"}}}\n' "$ATLAS_CORPUS"
  printf '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"corpus_ontology","arguments":{"corpus":"%s"}}}\n' "$ONTOLOGY_CORPUS"
  echo '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"corpus_list","arguments":{}}}'
} | "$CORPUS_MCP" --base-url "http://127.0.0.1:$PORT/v1" --corpus "$CORPUS" \
    >"$work/out.jsonl" 2>"$work/err.log" || { cat "$work/err.log" >&2; fail "corpus-mcp exited non-zero"; }

echo "--- corpus-mcp stderr ---"; cat "$work/err.log"; echo "-------------------------"
grep -q 'baseline OpenAI-compatible path' "$work/err.log" || fail "host was not detected as baseline — the test did not exercise the no-OICP path"

ontology_declared_on_disk=0
[[ -f "$DATA_ROOT/indexes/$ONTOLOGY_CORPUS/atlas/ontology.json" ]] && ontology_declared_on_disk=1
python3 - "$work/out.jsonl" "$CORPUS" "$ONTOLOGY_CORPUS" "$ontology_declared_on_disk" <<'PY'
import json, sys
out, corpus, ont_corpus, ont_on_disk = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1"
by_id = {}
for line in open(out):
    line = line.strip()
    if not line: continue
    m = json.loads(line); by_id[m.get("id")] = m
def result(i):
    m = by_id.get(i) or sys.exit(f"acceptance: FAIL — no response to id {i}")
    if "error" in m and m["error"]: sys.exit(f"acceptance: FAIL — id {i} errored: {m['error']}")
    return m["result"]
init = result(1); assert init["serverInfo"]["name"] == "corpus-mcp", init
tools = {t["name"] for t in result(2)["tools"]}
assert {"ask","corpus_list","corpus_search","atoms_lookup","corpus_ontology"} <= tools, tools
search = result(3)
assert not search.get("isError"), search["content"][0]["text"][:400]
rows = search["structuredContent"]["results"]
assert rows, "corpus_search returned no chunks"
cited = [r for r in rows if r.get("url") and r.get("corpus_id") == corpus and r.get("content")]
assert cited, f"no cited chunk with url+corpus+content: {rows[:2]}"
print(f"acceptance: corpus_search -> {len(rows)} chunks, {len(cited)} cited; top: {rows[0]['title']} {rows[0]['url']} score={rows[0]['score']:.4f}")
atoms = result(4)
assert not atoms.get("isError"), atoms["content"][0]["text"][:400]
arows = atoms["structuredContent"]["atoms"]
assert arows and all(a["kind"] == "claim" for a in arows), arows[:2]
print(f"acceptance: atoms_lookup -> {len(arows)} Claim atoms of {atoms['structuredContent']['total_atoms']} in {atoms['structuredContent']['corpus']}; first: {arows[0]['text'][:120]!r}")
ont = result(5)
print(f"acceptance: corpus_ontology({atoms['structuredContent']['corpus']}) -> {'declared' if not ont.get('isError') else 'absent, reported: ' + ont['content'][0]['text'][:120]}")
# The ontology read is judged on a corpus that DECLARED types. Until 2026-09-04
# the host parsed the AtlasOntologyFile envelope as bare policies and reported
# every declared ontology as "none"; this is the assertion that would have caught it.
ont2 = result(6)
if ont_on_disk:
    assert not ont2.get("isError"), ont2["content"][0]["text"][:400]
    types = ont2["structuredContent"]["policies"]["shape"]["types"]
    assert types, f"{ont_corpus} has ontology.json on disk but the host reports no declared types"
    print(f"acceptance: corpus_ontology({ont_corpus}) -> {len(types)} declared types: {', '.join(t['name'] for t in types)}")
else:
    print(f"acceptance: corpus_ontology({ont_corpus}) -> COULD-NOT-JUDGE (no {ont_corpus}/atlas/ontology.json installed here; set ONTOLOGY_CORPUS)")
listing = result(7)["structuredContent"]["corpora"]
row = next(r for r in listing if r["corpus_id"] == corpus)
print(f"acceptance: corpus_list -> {corpus}: {row['embedding_dimensions']}-d vector={row['vector_search']} atlas={row['has_atlas']} atoms={row['atom_count']} ontology_declared={row['ontology_declared']}")
PY

# ── 3c. `corpus ingest` — the WRITE half, against two bare processes ────────
#
# EPISTEMIC_INDEX.md §4 and §6 row 3. Everything above this line proves a
# third party can READ what our enrichment produced. This proves they can
# PRODUCE it: one recipe, two llama-server processes, no daemon, no GLiNER,
# structured output as plain `response_format: json_schema`.
#
# It is scored two ways, both against artefacts that already existed:
#   - `truth.json` recall, through the same recall table the daemon-built
#     hoard is judged by (`scripts/setup-numismatics-corpus.sh --atlas`).
#     The bar is the CONTROL's own row, printed beside it.
#   - `ask`, appended to ASK_CORPORA below, so the new corpus goes through
#     the identical mechanism assertions the installed fixtures do.
if [[ -z "$ACCEPT_INGEST" ]]; then
  echo "acceptance: corpus ingest -> NEVER-RAN (opt-in). To run it:"
  echo "    ACCEPT_INGEST=1 CHAT_GGUF=<an instruct .gguf> $0"
  echo "  It needs a SECOND llama-server (chat) beside the embed one and takes"
  echo "  ~20-40 min against a live model. It is not skipped — it has not run."
elif [[ -z "$CHAT_GGUF" ]]; then
  fail "ACCEPT_INGEST=1 without CHAT_GGUF — name the chat model to ingest with"
elif [[ ! -f "$CHAT_GGUF" ]]; then
  fail "CHAT_GGUF=$CHAT_GGUF does not exist"
else
  [[ "$INGEST_CORPUS" != "wessex-hoard" ]] || \
    fail "INGEST_CORPUS must not be wessex-hoard — that corpus is the control"

  # A staged copy of the committed fixture under a NEW id. The recipe and the
  # markdown are copied rather than edited in place, so the fixture the
  # control was built from is untouched and `truth.json` still describes both.
  stage="$work/$INGEST_CORPUS"
  mkdir -p "$stage"
  cp "$FIXTURE_DIR/wessex-hoard.md" "$stage/"
  sed "s/^id = \"wessex-hoard\"$/id = \"$INGEST_CORPUS\"/" \
    "$FIXTURE_DIR/recipe.toml" > "$stage/recipe.toml"
  grep -q "id = \"$INGEST_CORPUS\"" "$stage/recipe.toml" \
    || fail "staging the recipe did not rewrite [corpus] id"

  # The chat half. One model per llama-server process is exactly why
  # `corpus ingest` takes --chat-url and --embed-url apart.
  llama-server -m "$CHAT_GGUF" --host 127.0.0.1 --port "$CHAT_PORT" \
    >"$work/llama-chat.log" 2>&1 &
  chat_pid=$!
  for _ in $(seq 1 600); do
    curl -sf "http://127.0.0.1:$CHAT_PORT/health" >/dev/null 2>&1 && break
    kill -0 "$chat_pid" 2>/dev/null || { tail -40 "$work/llama-chat.log" >&2; fail "chat llama-server exited"; }
    sleep 1
  done
  curl -sf "http://127.0.0.1:$CHAT_PORT/health" >/dev/null || fail "chat llama-server never became healthy"
  echo "acceptance: chat frontend up on :$CHAT_PORT ($(basename "$CHAT_GGUF"))"

  ingest_argv=(ingest "$stage/recipe.toml"
    --chat-url "http://127.0.0.1:$CHAT_PORT/v1"
    --embed-url "http://127.0.0.1:$PORT/v1")
  if [[ -n "$INGEST_CHAPTERS" ]]; then
    ingest_argv+=(--chapters "$INGEST_CHAPTERS")
    echo "acceptance: FORECAST MODE — ingesting only [$INGEST_CHAPTERS];" \
         "the truth.json recall leg will report COULD-NOT-JUDGE"
  fi
  t_ingest=$(date +%s)
  set +e
  "$CORPUS_MCP" "${ingest_argv[@]}" 2>&1 | tee "$work/ingest.log"
  ingest_rc=${PIPESTATUS[0]}
  set -e
  ingest_secs=$(( $(date +%s) - t_ingest ))
  (( ingest_rc == 0 )) || fail "corpus ingest exited $ingest_rc after ${ingest_secs}s (see $work/ingest.log)"
  echo "acceptance: corpus ingest($INGEST_CORPUS) -> ok in ${ingest_secs}s"

  # The two degradations a bare endpoint has relative to the daemon must be
  # STATED by the run, not inferred from it (ARCH §18.3). If either line
  # stops being printed the claim "reported as such" is no longer true.
  grep -q 'GLiNER is NOT linked' "$work/ingest.log" \
    || fail "the run did not name the absent GLiNER entity pass"
  grep -q 'structured output = response_format json_schema' "$work/ingest.log" \
    || fail "the run did not name its structured-output mode"

  ing_atlas="$DATA_ROOT/indexes/$INGEST_CORPUS/atlas"
  [[ -f "$ing_atlas/ontology.json" ]] || fail "no ontology.json at $ing_atlas"
  [[ -f "$ing_atlas/atoms.json" ]]    || fail "no atoms.json at $ing_atlas"

  # THE BAR: the same recall table, on the same truth.json, for both atlases.
  # `--recipe-unchanged` because the staged recipe and the committed one have
  # checkout-fresh mtimes; the structural type-name comparison still runs.
  if [[ -n "$INGEST_CHAPTERS" ]]; then
    n_ch=$(tr ',' '\n' <<<"$INGEST_CHAPTERS" | grep -c .)
    echo "acceptance: truth.json recall -> COULD-NOT-JUDGE — this run ingested" \
         "$n_ch chapter(s) (INGEST_CHAPTERS), not the whole manifest the control" \
         "was built from; the recall table is not comparable. Wall measured:" \
         "${ingest_secs}s for $n_ch chapter(s)."
  else  # the real bar: the whole manifest, comparable to the control
  echo "--- truth.json recall: CONTROL (daemon-built wessex-hoard) ---"
  "$repo/scripts/setup-numismatics-corpus.sh" --atlas wessex-hoard --recipe-unchanged \
    2>&1 | tee "$work/recall-control.txt" || true
  echo "--- truth.json recall: THIS RUN ($INGEST_CORPUS, bare endpoints) ---"
  set +e
  "$repo/scripts/setup-numismatics-corpus.sh" --atlas "$INGEST_CORPUS" --recipe-unchanged \
    2>&1 | tee "$work/recall-bare.txt"
  recall_rc=$?
  set -e
  python3 - "$work/recall-control.txt" "$work/recall-bare.txt" "$recall_rc" <<'RECALLPY'
import re, sys
control, bare, rc = sys.argv[1], sys.argv[2], int(sys.argv[3])
def bars(path):
    out = {}
    for line in open(path):
        m = re.match(r"\s{2}(\S(?:.*?\S)?)\s+(\d+) / (\d+)\s+(ok|MISSED)", line)
        if m:
            out[m.group(1)] = (int(m.group(2)), int(m.group(3)), m.group(4))
    return out
c, b = bars(control), bars(bare)
if not c:
    sys.exit("acceptance: FAIL - the control produced no recall table to compare against")
if not b:
    sys.exit(f"acceptance: FAIL - the bare-endpoint run produced no recall table (exit {rc})")
worse = []
print(f"  {'bar':<22} {'bare':>9}  {'control':>9}")
for name, (cg, cw, _cs) in c.items():
    bg, bw, _bs = b.get(name, (0, cw, "MISSED"))
    print(f"  {name:<22} {bg:>4} / {bw:<4} {cg:>4} / {cw:<4}"
          + ("" if bg >= cg else "   <-- BELOW CONTROL"))
    if bg < cg:
        worse.append(f"{name}: {bg} vs {cg}")
if rc != 0:
    sys.exit(f"acceptance: FAIL - the bare-endpoint atlas missed a truth.json bar (exit {rc})")
if worse:
    sys.exit("acceptance: FAIL - recall below the daemon-built control on: " + "; ".join(worse))
print("acceptance: corpus ingest -> truth.json recall >= the daemon-built control on every bar")
RECALLPY
  fi
  # ── the §6 row-3 bar for THIS corpus: attribution claims, cited, connected ──
  #
  # The recall table above says the declared nouns reached the atoms. This
  # says they reached an ANSWER: a question about a disputed dating must come
  # back with cited passages, with `coin` and `attribution` among the idea
  # nodes the walk traversed, and with a Tension or Grounds edge followed —
  # the connective tissue the whole index exists for. Asked the way a reader
  # asks, not in the ontology's own words (ARCH §18.1).
  ATTRIBUTION_QUERY="${ATTRIBUTION_QUERY:-Which coins in this hoard have disputed datings, and what evidence do the scholars give?}"
  {
    echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"acceptance","version":"0"}}}'
    echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask","arguments":{"question":"%s","corpus":"%s"}}}\n' "$ATTRIBUTION_QUERY" "$INGEST_CORPUS"
  } | "$CORPUS_MCP" --base-url "http://127.0.0.1:$PORT/v1" --corpus "$INGEST_CORPUS" \
      >"$work/ask-ingest.jsonl" 2>"$work/ask-ingest.err" \
      || { cat "$work/ask-ingest.err" >&2; fail "corpus-mcp exited non-zero on ask($INGEST_CORPUS)"; }
  python3 - "$work/ask-ingest.jsonl" "$INGEST_CORPUS" "$ing_atlas" <<'INGESTASKPY'
import collections, json, sys
out, corpus, atlas = sys.argv[1], sys.argv[2], sys.argv[3]

# ── the precondition, read off the atlas this run just built, before judging ──
# 3b's discipline applied here: what the walk can surface is bounded by what
# the atlas holds and by which kinds the seed table carries, and asserting
# through either would report the corpus's state as the host's.
atoms = json.load(open(f"{atlas}/atoms.json"))["atoms"]
edges = json.load(open(f"{atlas}/edges.json"))
edges = edges["edges"] if isinstance(edges, dict) else edges
ontology = json.load(open(f"{atlas}/ontology.json"))
def tag(a):
    d = a["data"]
    return d.get("entity_type") or d.get("claim_kind") or a["atom_type"]
have = collections.Counter(tag(a) for a in atoms)
edge_kinds = collections.Counter((e.get("edge_type") or e.get("kind")) for e in edges)
coins = have["coin"] + have["sceatta"]
attributions = have["attribution"]
connective_on_disk = {k for k in ("Tension", "Grounds") if edge_kinds.get(k)}
# Which kinds the ANN seed table was populated with. Absent = the entity-only
# table every corpus built before ei-3 has, on which a `tension` row (Claim +
# Position) seeds nothing however good the walk is.
try:
    seeded_kinds = set(open(f"{atlas}/atoms_ann.population").read().split("\n")[1].split(","))
except (OSError, IndexError):
    seeded_kinds = set()

by_id = {}
for line in open(out):
    line = line.strip()
    if not line: continue
    m = json.loads(line); by_id[m.get("id")] = m
m = by_id.get(2) or sys.exit(f"acceptance: FAIL - no response to ask({corpus})")
if m.get("error"): sys.exit(f"acceptance: FAIL - ask({corpus}) errored: {m['error']}")
r = m["result"]
assert not r.get("isError"), r["content"][0]["text"][:400]
s = r["structuredContent"]
mp, passages = s["map"], s["passages"]
subtypes = {n.get("subtype", "") for n in mp["nodes"]}
edges_followed = set(mp["edge_kinds"])
cited = [p for p in passages if p.get("content") and p.get("title")]

print(f"acceptance: ask({corpus}) -> kind={mp['question_kind']} ({mp['question_kind_source']}), "
      f"{mp['nodes_reached']} nodes, subtypes={sorted(t for t in subtypes if t)}, "
      f"edges={sorted(edges_followed)}, {len(cited)}/{len(passages)} cited passages "
      f"[on disk: {coins} coin, {attributions} attribution, edges {sorted(connective_on_disk)}, "
      f"seed kinds {sorted(seeded_kinds) or 'entity-only/unrecorded'}]")
for d in s["degradations"]:
    print(f"acceptance:   degraded - {d}")

# ── MECHANISM (hard, unconditional) ──
# The answer is CITED, the question reached a row by centroid, and the map is
# not empty. This is the half that has nothing to do with which edges the
# corpus happens to hold: a bare-endpoint ingest that produced an unsearchable
# index, an unclassifiable question or an atlas nothing can seed fails here.
assert cited, "ask returned no passage with both a title and content"
assert mp["question_kind_source"] == "classified", (
    f"the question was not classified onto a row: {mp['question_kind_source']}")
assert mp["nodes"], (
    f"ask returned no map nodes at all (walk={s['walk']}, degraded={s['degradations']}). "
    f"The atlas holds {len(atoms)} atoms and the seed table carries "
    f"{sorted(seeded_kinds) or 'entity-only/unrecorded'}.")

# ── THE DECLARATION REACHED AN ANSWER (hard) ──
# `attribution` is this recipe's own claim type. Seeing it in the map is the
# whole ontology-v1 claim, restated on the retrieval side: the author's noun,
# extracted by a bare endpoint, surfaced as an idea node behind a cited answer.
assert "attribution" in subtypes, (
    f"the map names no `attribution` node, though the atlas holds {attributions}. "
    f"Seed kinds: {sorted(seeded_kinds) or 'entity-only (pre-ei-3 table)'}; "
    f"degradations: {s['degradations']}")

# ── THE CONNECTED HALF (judged only where the row and the corpus can meet) ──
# `ask` walks the edge kinds the corpus's own map declares for the row the
# question classified onto. The `tension` row's §2.2 default walks Tension and
# OpposesIn; this fixture's connective edges are Involves and Grounds, which
# that row does not traverse — so from an attribution claim there is no hop to
# the `coin` it involves, however good the extraction was. Widening the row to
# make this bar go green would be tuning a navigation default to the bench,
# which the campaign forbids; the honest verdict is COULD-NOT-JUDGE naming
# both sets, and it is a contract gap between the §2.2 defaults and what a
# declared ontology actually produces (EPISTEMIC_INDEX.md §7 step 5).
row = ontology["policies"].get("navigation", {}).get(mp["question_kind"], {})
row_walks = set(row.get("walk") or [])
reachable = row_walks & set(edge_kinds)
if not reachable:
    print(f"acceptance: ask({corpus}) connected bar -> COULD-NOT-JUDGE: the "
          f"`{mp['question_kind']}` row walks {sorted(row_walks) or 'nothing'} and this atlas "
          f"holds {sorted(edge_kinds)}. No edge of a kind the row traverses exists, so no hop "
          f"can be made and `coin` cannot be reached from `attribution`. This is the §2.2 "
          f"navigation default meeting a declared ontology's edge vocabulary; it is NOT "
          f"waived, and the mechanism + declaration bars above ran.")
elif not coins:
    print(f"acceptance: ask({corpus}) connected bar -> COULD-NOT-JUDGE: the atlas holds no "
          f"`coin` atom to reach ({attributions} attribution(s) present). An extraction "
          f"result, reported by the recall table above.")
else:
    assert "coin" in subtypes, (
        f"the map names no `coin` node, though the atlas holds {coins} and the "
        f"`{mp['question_kind']}` row walks {sorted(reachable)}, which this atlas has. "
        f"Degradations: {s['degradations']}")
    followed = edges_followed & {"Tension", "Grounds"}
    assert followed, (
        f"no Tension or Grounds edge was followed, though the atlas holds "
        f"{sorted(connective_on_disk)} and the row walks {sorted(reachable)}; "
        f"edges followed: {sorted(edges_followed)}; degradations: {s['degradations']}")
    print(f"acceptance: ask({corpus}) -> coin + attribution in the map, connected by "
          f"{sorted(followed)}, over {len(cited)} cited passage(s)")
INGESTASKPY

  # …and the read half, on the corpus this run just wrote.
  ASK_CORPORA="$ASK_CORPORA $INGEST_CORPUS"
  kill "$chat_pid" 2>/dev/null || true
  chat_pid=""
fi

# ── 3b. `ask` — the composed default, on every fixture installed here ───────
#
# `EPISTEMIC_INDEX.md` §4 and §6 row 3. Two claims, judged separately:
#
#   MECHANISM (asserted on every corpus that has an atlas): `ask` classifies
#   the question onto a navigation row by centroid, walks, and returns a
#   non-empty map whose nodes account for every passage the walk cited. This
#   half goes red if the tool stops working.
#
#   THE THEME BAR (>=3 themes each with a cited passage): asserted only where
#   the FIXTURE can carry it, and the precondition is read off the atlas on
#   disk BEFORE the call, never inferred from the result. Today's ANN seed
#   table is Entity-only — `writer::seed_atlas` seeds through
#   `AtlasContextFilter::default()`, whose `include_configurations` and
#   `include_tensions` are `false` — so a corpus whose themes are
#   `Configuration` atoms cannot seed them however good the walk is. That is a
#   seed-coverage fact (ei-3), and asserting through it would report the
#   corpus's state as the host's.
#
# Each corpus is run in its OWN host process, because the host serves the
# corpora it was opened with and each walk needs its own atlas.
for ask_corpus in $ASK_CORPORA; do
  if [[ ! -d "$DATA_ROOT/indexes/$ask_corpus" ]]; then
    echo "acceptance: ask($ask_corpus) -> COULD-NOT-JUDGE (not installed here)"
    continue
  fi
  {
    echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"acceptance","version":"0"}}}'
    echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask","arguments":{"question":"%s","corpus":"%s"}}}\n' "$THEMATIC_QUERY" "$ask_corpus"
  } | "$CORPUS_MCP" --base-url "http://127.0.0.1:$PORT/v1" --corpus "$ask_corpus" \
      >"$work/ask-$ask_corpus.jsonl" 2>"$work/ask-$ask_corpus.err" \
      || { cat "$work/ask-$ask_corpus.err" >&2; fail "corpus-mcp exited non-zero on ask($ask_corpus)"; }
  python3 - "$work/ask-$ask_corpus.jsonl" "$ask_corpus" \
           "$DATA_ROOT/indexes/$ask_corpus/atlas/atoms.json" <<'ASKPY'
import json, sys
out, corpus, atoms_path = sys.argv[1], sys.argv[2], sys.argv[3]

# ── the fixture precondition, read off disk, before anything is judged ──
# How many atoms this corpus has that the thematic row can BOTH seed on and
# cite: an Entity of entity_type `concept` (the literary pipeline's `theme`,
# per `pipelines/ontologies/literary_atlas.toml`) carrying an evidence anchor.
# Configurations are counted apart and NOT included: the ANN seed table is
# entity-only today, so they can be in the map's row and still be unseedable.
seedable, configurations = 0, 0
try:
    for a in json.load(open(atoms_path))["atoms"]:
        d = a["data"]
        has_evidence = bool(d.get("evidence")) or bool(d.get("first_appearance"))
        if a["atom_type"] == "Configuration":
            configurations += 1
        elif a["atom_type"] == "Entity" and d.get("entity_type") == "concept" and has_evidence:
            seedable += 1
except FileNotFoundError:
    pass

by_id = {}
for line in open(out):
    line = line.strip()
    if not line: continue
    m = json.loads(line); by_id[m.get("id")] = m
m = by_id.get(2) or sys.exit(f"acceptance: FAIL - no response to ask({corpus})")
if m.get("error"): sys.exit(f"acceptance: FAIL - ask({corpus}) errored: {m['error']}")
r = m["result"]
assert not r.get("isError"), r["content"][0]["text"][:400]
s = r["structuredContent"]
mp, passages, walk = s["map"], s["passages"], s["walk"]

# ── MECHANISM (hard, every corpus with an atlas) ──
# 1. The question reached a row by centroid, not by abstention. A thematic
#    question that abstains means the classifier stopped discriminating.
assert mp["question_kind"] == "thematic", f"expected the thematic row, got {mp['question_kind']}"
assert mp["question_kind_source"] == "classified", (
    f"the thematic question was not classified: {mp['question_kind_source']}")
# 2. The map is non-empty - §4's "the idea nodes traversed, their kinds, and
#    the edges followed". An `ask` with passages and no map never touched the
#    index of ideas, which is the whole claim under test.
assert mp["nodes"], f"ask returned no map nodes (walk={walk}, degraded={s['degradations']})"
# 3. Every idea a passage claims to evidence is IN the map. This is the link
#    that makes the map checkable rather than decorative; without it a
#    passage could name any atom at all.
in_map = {n["atom_id"] for n in mp["nodes"]}
for p in passages:
    for a in p.get("motivating_atoms") or []:
        assert a in in_map, f"passage cites {a}, which the map does not list"

themes = {n["atom_id"]: n for n in mp["nodes"]
          if n["kind"] == "configuration" or (n["kind"] == "entity" and n["subtype"] == "concept")}
cited = {}
for p in passages:
    if not p.get("content"): continue
    for a in p.get("motivating_atoms") or []:
        if a in themes: cited.setdefault(a, p)

print(f"acceptance: ask({corpus}) -> kind={mp['question_kind']} ({mp['question_kind_source']}, "
      f"{mp['policy']}), {mp['nodes_reached']} nodes, edges={mp['edge_kinds']}, "
      f"{len(passages)} passages ({sum(1 for p in passages if 'walk' in p['origin'])} cited by the walk), "
      f"{len(themes)} themes in the map, {len(cited)} with a cited passage "
      f"[seedable themes on disk: {seedable}; configurations: {configurations}]")
for d in s["degradations"]:
    print(f"acceptance:   degraded - {d}")

# ── THE THEME BAR (hard where the fixture can carry it) ──
BAR = 3
if seedable >= BAR:
    assert len(cited) >= BAR, (
        f"ask({corpus}) surfaced {len(cited)} themes with a cited passage, need >={BAR}; "
        f"the atlas holds {seedable} seedable ones, so this is the walk, not the fixture. "
        f"walk={walk} degraded={s['degradations']}")
    print(f"acceptance: ask({corpus}) -> {len(cited)}/{BAR} themes cited with evidence: "
          + "; ".join(f"{themes[a]['name']!r} <- {p['title']}" for a, p in list(cited.items())[:BAR]))
else:
    print(f"acceptance: ask({corpus}) theme bar -> COULD-NOT-JUDGE: the atlas holds {seedable} "
          f"seedable theme atom(s), under the bar of {BAR}, so this corpus cannot exercise it "
          f"({configurations} Configuration atom(s) are excluded because the ANN seed table is "
          f"entity-only - AtlasContextFilter::default(), writer::seed_atlas). The bar goes live "
          f"here the moment seed coverage widens; it is NOT waived, and the mechanism assertions "
          f"above ran.")
ASKPY
done

# ── 4. the closure ──────────────────────────────────────────────────────────
if command -v cargo >/dev/null; then
  bad="$(cargo tree -p corpus-mcp -e normal --prefix none 2>/dev/null | awk '{print $1}' | sort -u \
         | grep -E '^(llama-cpp-4|llama-cpp-sys-4|ort|ort-sys|iroh|sovereign-inference|sovereign-gliner|commonwealth-transport|sovereign-core|sovereign-tools)$' || true)"
  [[ -z "$bad" ]] || fail "dep tree carries: $bad"
  echo "acceptance: cargo tree -p corpus-mcp: no llama.cpp / ort / iroh / mesh transport / runtime"
else
  echo "acceptance: cargo not on PATH here — closure checked by tests/no_inference_stack.rs instead"
fi
if command -v ldd >/dev/null; then
  ldd "$CORPUS_MCP" | grep -iE 'llama|ggml|onnx' && fail "binary links an inference shared library" || true
fi
echo "acceptance: PASS"
