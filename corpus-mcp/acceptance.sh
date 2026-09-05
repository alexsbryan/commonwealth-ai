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
#   3d. pull-if-absent — `corpus serve --corpus sep` on a COLD, isolated data
#      root installs the corpus from its prebuilt HF snapshot and then serves
#      a cited answer out of it. ~875 MB of egress, so opt-in via ACCEPT_PULL=1
#      and NEVER-RAN by name otherwise (ARCH §18.2);
#   4. the dep tree, asserted free of llama.cpp / ort / iroh.
#
# Steps 0 and 0b run BEFORE any model loads, because neither needs one:
#   0.  `corpus recipe new` — the FIRST of §4's three commands: it scaffolds
#      from the numismatics template, substitutes --id, leaves the source path
#      for the author, and refuses to overwrite. Then the endpoint discovery
#      ladder (order ei-6-distribution): every rung named whichever way it
#      goes, and a `--base-url` that does not answer REFUSED rather than
#      swapped for one that does (ARCH §18.3);
#   0b. Ollama — §4's default shape. Reported PASS or COULD-NOT-RUN by name,
#      never skipped; where it runs, the embedding-width question §7 step 6
#      asks is answered from the real index.
#
# Env: OLLAMA_URL (default http://localhost:11434/v1), ACCEPT_PULL / PULL_ROOT /
#      PULL_CORPUS (the cold-root pull leg),
#      EMBED_GGUF (default sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf),
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
# Resolved ONCE, absolute: the `recipe new` leg runs in a scratch directory
# (the scaffold must not land in the repo) and a relative path would not
# survive the `cd`. Done here so there is one spelling of "the binary".
[[ "$CORPUS_MCP" = /* ]] || CORPUS_MCP="$repo/$CORPUS_MCP"
# Ollama's OpenAI-compatible surface — the first rung of the discovery ladder
# and the shape §4's commands are written for. Probed, never assumed.
OLLAMA_URL="${OLLAMA_URL:-http://localhost:11434/v1}"
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
# Every external tool a LATER leg needs, checked HERE. Run 1 of stage 2
# (2026-09-05) spent 5,418 s on the ingest and then died in the scorer on
# `FATAL: jq is required` — jq is on this host and was not in the toolbox the
# run was launched into. A dependency check that runs after the expensive step
# is not a preflight, it is an autopsy: the artifact survived and was scorable
# post hoc, but the run's own verdict was lost. Named individually so the
# refusal says which one (ARCH §18.3).
for tool in jq python3; do
  command -v "$tool" >/dev/null \
    || fail "$tool not on PATH — the truth.json recall leg needs it, and this refuses now rather than after the ingest"
done

# ── 0. the three commands, and the ladder — no model needed ─────────────────
#
# EPISTEMIC_INDEX.md §4 is `recipe new` -> `ingest` -> `serve`, and the first
# and third of those are judged here because neither needs an endpoint. They
# run FIRST for that reason: a broken verb should cost a second, not the
# minutes it takes llama-server to load (ARCH §18.4, validate the instrument
# before the result).

scaffold_dir="$work/scaffold"
mkdir -p "$scaffold_dir"
( cd "$scaffold_dir" && "$CORPUS_MCP" recipe new --ontology numismatics --id my-coins ) \
  >"$work/recipe-new.out" 2>"$work/recipe-new.err" \
  || { cat "$work/recipe-new.err" >&2; fail "recipe new exited non-zero"; }
[[ -f "$scaffold_dir/my-coins.toml" ]] || fail "recipe new wrote no my-coins.toml"
grep -q '^id = "my-coins"' "$scaffold_dir/my-coins.toml" \
  || fail "recipe new did not substitute --id into the scaffold"
# The source path is the ONE thing only the author knows; a scaffold that
# guessed it would ingest the wrong directory without asking. It stays.
grep -q 'path = "REPLACE_ME"' "$scaffold_dir/my-coins.toml" \
  || fail "recipe new no longer leaves the source path for the author"
echo "acceptance: recipe new -> wrote my-coins.toml from the numismatics template"

# Never overwrites. The one destructive mistake this verb could make.
echo "MINE" > "$scaffold_dir/my-coins.toml"
if ( cd "$scaffold_dir" && "$CORPUS_MCP" recipe new --ontology numismatics --id my-coins ) \
     >/dev/null 2>"$work/recipe-clobber.err"; then
  fail "recipe new overwrote an existing recipe"
fi
grep -q 'never overwrites' "$work/recipe-clobber.err" \
  || fail "the overwrite refusal does not say why: $(cat "$work/recipe-clobber.err")"
[[ "$(cat "$scaffold_dir/my-coins.toml")" == "MINE" ]] \
  || fail "the refusal still modified the file"
echo "acceptance: recipe new -> refuses to overwrite, and did not touch the file"

# The discovery ladder. Pointed at ports nothing serves, so what is asserted is
# the REPORT, not whatever happens to be up on the machine running this.
if SOVEREIGN_DAEMON_URL="http://127.0.0.1:1" "$CORPUS_MCP" serve </dev/null \
     >/dev/null 2>"$work/ladder.err"; then
  fail "serve succeeded with no endpoint reachable anywhere"
fi
for rung in ollama llama-server "oicp daemon" 11434 8080; do
  grep -q -- "$rung" "$work/ladder.err" \
    || fail "the discovery ladder did not name the '$rung' rung: $(cat "$work/ladder.err")"
done
grep -q 'no inference endpoint found' "$work/ladder.err" \
  || fail "an absent endpoint was not reported as an absence (ARCH §18.3)"
echo "acceptance: discovery -> all three rungs probed and named, absence reported"

# ARCH §18.3, the rule that matters most here: an endpoint the caller NAMED is
# refused when it does not answer, never swapped for one that does. A
# fall-through would serve a different model, successfully and silently.
if "$CORPUS_MCP" serve --base-url "http://127.0.0.1:1/v1" </dev/null \
     >/dev/null 2>"$work/named.err"; then
  fail "a dead --base-url did not refuse"
fi
grep -q -- '--base-url' "$work/named.err" \
  || fail "the refusal does not name the flag: $(cat "$work/named.err")"
for other in 11434 8080; do
  grep -q -- "$other" "$work/named.err" \
    && fail "a named --base-url fell through to the ladder (found $other)"
done
echo "acceptance: discovery -> a named --base-url is refused, never substituted"

# ── 0b. Ollama ──────────────────────────────────────────────────────────────
#
# §4 names Ollama as the shape the three commands are written for (one
# process, one URL, both models). Whether it is HERE is a property of the
# machine, so this reports which of the two it is BY NAME and never silently
# skips (ARCH §18.2: four verdicts, not two).
if command -v ollama >/dev/null && curl -sf -m 3 "$OLLAMA_URL/models" >/dev/null 2>&1; then
  ollama_models="$(curl -s -m 5 "$OLLAMA_URL/models")"
  echo "acceptance: ollama -> reachable at $OLLAMA_URL"
  { echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"acceptance","version":"0"}}}'
    echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"corpus_list","arguments":{}}}'
  } | "$CORPUS_MCP" serve --base-url "$OLLAMA_URL" --corpus "$CORPUS" \
      >"$work/ollama.jsonl" 2>"$work/ollama.err" \
      || { cat "$work/ollama.err" >&2; fail "corpus-mcp against Ollama exited non-zero"; }
  grep -q 'corpus_id' "$work/ollama.jsonl" \
    || fail "corpus_list returned nothing against Ollama: $(cat "$work/ollama.err")"
  # The width question §7 step 6 asks by name: an Ollama embedding whose width
  # does not match the shipped index degrades to full-text and SAYS so. Which
  # way it went is reported either way; neither is a failure of this host.
  if grep -q 'vector search DISABLED' "$work/ollama.err"; then
    echo "acceptance: ollama -> WIDTH MISMATCH against $CORPUS; degraded to full-text and said so"
  else
    echo "acceptance: ollama -> embedding width matches $CORPUS; vector + full-text live"
  fi
  echo "acceptance: ollama arm -> PASS"
else
  echo "acceptance: ollama arm -> COULD-NOT-RUN (not installed or not serving at $OLLAMA_URL)."
  echo "acceptance:   This is NOT a pass and NOT a fail. The llama-server arm below is the"
  echo "acceptance:   measured one; the Ollama default is a documented option until a host"
  echo "acceptance:   with Ollama runs this. To run it: \`ollama serve\`, then re-run."
fi

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

# ── 3d. pull-if-absent on a COLD data root ──────────────────────────────────
#
# EPISTEMIC_INDEX.md §1's Distribution row: `corpus serve --corpus <id>` on a
# machine that has never held the corpus installs it first, from the prebuilt
# snapshot its recipe declares — chunks AND atlas in one archive, not a
# re-embed.
#
# Opt-in, because it is ~875 MB of egress from HuggingFace. Absent the opt-in
# it reports NEVER-RAN by name (ARCH §18.2) rather than being silently skipped
# — the same shape as the ingest leg above.
#
# The root is ISOLATED and must be COLD. Two reasons it is not $work: $work is
# under /tmp, which is tmpfs on the development host (a 875 MB pull would be
# 875 MB of RAM on a box whose daemon is the kernel's first OOM victim), and a
# root that already holds `sep` would make this leg pass without pulling
# anything — the thing it exists to prove.
PULL_ROOT="${PULL_ROOT:-$repo/test-artifacts/ei6-pull-root}"
PULL_CORPUS="${PULL_CORPUS:-$CORPUS}"
if [[ -z "${ACCEPT_PULL:-}" ]]; then
  echo "acceptance: pull-if-absent -> NEVER-RAN (opt-in; ~875 MB of egress). To run it:"
  echo "acceptance:   ACCEPT_PULL=1 $0"
else
  [[ -e "$PULL_ROOT" ]] && fail "PULL_ROOT $PULL_ROOT already exists — this leg needs a COLD root; remove it or set PULL_ROOT"
  mkdir -p "$PULL_ROOT"
  echo "acceptance: pull-if-absent -> cold root $PULL_ROOT, pulling \`$PULL_CORPUS\`"
  # SOVEREIGN_DATA_DIR and not --data-dir: the enrichment store derives its own
  # root from the env var, so the flag alone would put the two halves of one
  # corpus in two roots. `corpus ingest` refuses that disagreement by name; here
  # we simply set the thing both halves read.
  { echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"acceptance","version":"0"}}}'
    echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"corpus_search","arguments":{"query":"%s","corpus":"%s"}}}\n' "$QUERY" "$PULL_CORPUS"
    echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"corpus_list","arguments":{}}}'
  } | SOVEREIGN_DATA_DIR="$PULL_ROOT" "$CORPUS_MCP" serve \
        --base-url "http://127.0.0.1:$PORT/v1" --corpus "$PULL_CORPUS" \
        >"$work/pull.jsonl" 2>"$work/pull.err" \
    || { cat "$work/pull.err" >&2; fail "serve on a cold root exited non-zero"; }
  grep -q 'is not installed — pulling the prebuilt snapshot' "$work/pull.err" \
    || fail "the cold root did not pull — it was not cold, or the pull was silent: $(cat "$work/pull.err")"
  [[ -d "$PULL_ROOT/indexes/$PULL_CORPUS" ]] \
    || fail "the pull reported success but wrote no index under $PULL_ROOT/indexes/$PULL_CORPUS"
  # SERVES, not just installs: the done-when is "pulls and serves", so the
  # proof is a cited chunk out of the corpus that was not there a minute ago.
  python3 - "$work/pull.jsonl" "$PULL_CORPUS" <<'PULLPY'
import json, sys
out, corpus = sys.argv[1], sys.argv[2]
by_id = {}
for line in open(out):
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue
    if "id" in msg:
        by_id[msg["id"]] = msg
search = by_id.get(2, {}).get("result", {})
text = "".join(c.get("text", "") for c in search.get("content", []))
if search.get("isError") or not text.strip():
    sys.exit(f"acceptance: FAIL — corpus_search on the pulled `{corpus}` returned nothing: {search}")
# A citation, not just prose: the whole claim of the snapshot is that what
# arrives is a searchable corpus.
if corpus not in text and "chunk" not in text.lower():
    sys.exit(f"acceptance: FAIL — the pulled corpus answered without citing anything:\n{text[:400]}")
print(f"acceptance: pull-if-absent -> `{corpus}` pulled onto a cold root and SERVED a cited answer")
PULLPY
  rm -rf "$PULL_ROOT"
fi

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
