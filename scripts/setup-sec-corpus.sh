#!/usr/bin/env bash
#
# setup-sec-corpus.sh — one-command setup of a single-company SEC Filings
# Corpus (Form 10-K prose + XBRL companyfacts figures), installed under the
# machine-stable corpus_id `sec-cik<10-digit-CIK>`.
#
# Adapted from setup-proxy-corpus.sh (same resolve/discover/fetch/materialize
# pipeline, same EDGAR URL synthesis, same HTML->text cleaning); differences:
#   - form type 10-K (latest in window, or --accession pin)
#   - a second acquire path: data.sec.gov companyfacts JSON, rendered to
#     per-concept fact .txt files by scripts/sec_facts.py (THE one decider,
#     driven by sovereign-recipes/sec-filings-company/concept-map.toml)
#   - every 10-K in the window is LISTED, and every one not selected is
#     NAMED as skipped — a silent skip fails the order's B1 bar.
#
# Prerequisites: jq, curl, python3 (>=3.11 for tomllib); for the install step,
# a reachable daemon with the embed model loaded. --skip-install stops after
# download + render + materialize (iterate with `recipe test`, no model needed).
#
# Usage:
#   scripts/setup-sec-corpus.sh <TICKER|CIK> [--bin <cli>] [--from YYYY-MM-DD]
#                               [--to YYYY-MM-DD] [--accession ACC]
#                               [--fy N (repeatable; default latest 3)]
#                               [--skip-install]

set -euo pipefail

CONTACT_UA="commonwealth-ai/0.1 (sec-filings-corpus; alexbryan01@gmail.com)"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CANONICAL_RECIPE="${REPO_ROOT}/sovereign-recipes/sec-filings-company/recipe.toml"
CONCEPT_MAP="${REPO_ROOT}/sovereign-recipes/sec-filings-company/concept-map.toml"
DECIDER="${REPO_ROOT}/scripts/sec_facts.py"
CACHE_DIR="${HOME}/.svrnmesh/cache/sec"
TICKERS_JSON="${CACHE_DIR}/company_tickers.json"
FROM_DATE="2024-01-01"
TO_DATE="2026-12-31"
ACCESSION=""
SKIP_INSTALL=""
FY_ARGS=()
ARG=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --from) FROM_DATE="$2"; shift 2 ;;
    --to) TO_DATE="$2"; shift 2 ;;
    --accession) ACCESSION="$2"; shift 2 ;;
    --fy) FY_ARGS+=(--fy "$2"); shift 2 ;;
    --skip-install) SKIP_INSTALL=1; shift ;;
    -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
    -*) echo "unknown flag: $1" >&2; exit 2 ;;
    *) ARG="$1"; shift ;;
  esac
done

command -v jq      >/dev/null 2>&1 || { echo "FATAL: jq is required" >&2; exit 2; }
command -v curl    >/dev/null 2>&1 || { echo "FATAL: curl is required" >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || { echo "FATAL: python3 is required" >&2; exit 2; }
[[ -n "$ARG" ]] || { echo "FATAL: pass a ticker (e.g. AAPL) or a CIK" >&2; exit 2; }
[[ -f "$CANONICAL_RECIPE" ]] || { echo "FATAL: template recipe not found at $CANONICAL_RECIPE" >&2; exit 2; }
[[ -f "$CONCEPT_MAP" ]] || { echo "FATAL: concept map not found at $CONCEPT_MAP" >&2; exit 2; }
[[ -f "$DECIDER" ]] || { echo "FATAL: decider not found at $DECIDER" >&2; exit 2; }

mkdir -p "$CACHE_DIR"

# ── 1. Resolve ticker -> CIK (numeric arg is taken as a CIK as-is) ───────────
TICKER=""
if [[ "$ARG" =~ ^[0-9]+$ ]]; then
  CIK_RAW="$ARG"
  TITLE="CIK ${ARG}"
else
  TICKER="$(printf '%s' "$ARG" | tr '[:lower:]' '[:upper:]')"
  if [[ ! -s "$TICKERS_JSON" ]]; then
    echo "fetching SEC company_tickers.json (cached at $TICKERS_JSON) …"
    curl -s --max-time 60 -H "User-Agent: $CONTACT_UA" \
      "https://www.sec.gov/files/company_tickers.json" -o "$TICKERS_JSON"
  fi
  TUP="$(jq -r --arg t "$TICKER" '
    [ to_entries[] | select(.value.ticker == $t) | .value ][0]
    | if . == null then empty else "\(.cik_str)\t\(.title)" end' "$TICKERS_JSON")"
  [[ -n "$TUP" ]] || { echo "FATAL: ticker '$ARG' not found in company_tickers.json" >&2; exit 1; }
  IFS=$'\t' read -r CIK_RAW TITLE <<<"$TUP"
fi

CIK10="$(printf '%010d' "$((10#$CIK_RAW))")"   # zero-padded → corpus_id + APIs
CIK_BARE="$((10#$CIK_RAW))"                     # leading zeros stripped → archives path
CORPUS_ID="sec-cik${CIK10}"
echo "resolved: ${ARG} → ${TITLE} (CIK ${CIK10}) → corpus_id ${CORPUS_ID}"

# ── 2. Discover 10-K filings; select ONE, NAME every skip (B1) ───────────────
if [[ -n "$ACCESSION" ]]; then
  echo "discovering 10-K by pinned accession ${ACCESSION} …"
else
  echo "discovering 10-K filings in [${FROM_DATE} .. ${TO_DATE}] …"
fi
SUBS="$(curl -s --max-time 60 -H "User-Agent: $CONTACT_UA" \
  "https://data.sec.gov/submissions/CIK${CIK10}.json")"
ALL_HITS="$(printf '%s' "$SUBS" | jq -r --arg from "$FROM_DATE" --arg to "$TO_DATE" --arg acc "$ACCESSION" '
  .filings.recent as $r
  | [ range(0; ($r.form | length))
      | select($r.form[.] == "10-K"
               and ( if $acc != "" then $r.accessionNumber[.] == $acc
                     else ($r.filingDate[.] >= $from and $r.filingDate[.] <= $to) end ))
      | {acc: $r.accessionNumber[.], doc: $r.primaryDocument[.], date: $r.filingDate[.]} ]
  | sort_by(.date) | .[] | "\(.acc)\t\(.doc)\t\(.date)"')"
[[ -n "$ALL_HITS" ]] || {
  if [[ -n "$ACCESSION" ]]; then
    echo "FATAL: no 10-K with accession ${ACCESSION} for CIK ${CIK10} in recent filings." >&2
  else
    echo "FATAL: no 10-K for CIK ${CIK10} in [${FROM_DATE} .. ${TO_DATE}]. Widen --from/--to." >&2
  fi
  exit 1
}
SELECTED="$(printf '%s\n' "$ALL_HITS" | tail -1)"
IFS=$'\t' read -r ACC DOC FDATE <<<"$SELECTED"
while IFS=$'\t' read -r a d dt_; do
  if [[ "$a" == "$ACC" ]]; then
    echo "  selected: 10-K accession ${a}  filed ${dt_}  primary ${d}"
  else
    echo "  SKIPPED (not latest in window): 10-K accession ${a} filed ${dt_}"
  fi
done <<<"$ALL_HITS"
ACC_NODASH="${ACC//-/}"
DOC_URL="https://www.sec.gov/Archives/edgar/data/${CIK_BARE}/${ACC_NODASH}/${DOC}"

# ── 3. Fetch the 10-K primary document, clean HTML -> prose part files ───────
SEC_DIR="${HOME}/.svrnmesh/cache/sec-filings/${CORPUS_ID}"
RAW_DIR="${SEC_DIR}/raw"
DOCS_DIR="${SEC_DIR}/docs"
PROSE_DIR="${DOCS_DIR}/prose"
FACTS_DIR="${DOCS_DIR}/facts"
mkdir -p "$RAW_DIR" "$PROSE_DIR" "$FACTS_DIR"
rm -f "${PROSE_DIR}"/*.txt "${FACTS_DIR}"/*.txt "${FACTS_DIR}"/_*.json 2>/dev/null || true
RAW="${RAW_DIR}/${ACC_NODASH}.html"
if [[ -s "$RAW" ]]; then
  echo "  cached raw filing: $RAW"
else
  echo "  fetching: $DOC_URL"
  curl -s --max-time 120 -H "User-Agent: $CONTACT_UA" "$DOC_URL" -o "$RAW"
  [[ -s "$RAW" ]] || { echo "FATAL: skipped filing ${ACC}: download produced an empty file" >&2; exit 1; }
fi
echo "  raw bytes: $(wc -c < "$RAW" | tr -d ' ')"
# Same cleaning as setup-proxy-corpus.sh (see its header + the template recipe
# for the rationale); threshold 40 to also catch mid-length inline-XBRL ids.
python3 - "$RAW" "$PROSE_DIR" "$ACC_NODASH" <<'PY'
import sys, re, html
raw = open(sys.argv[1], encoding="utf-8", errors="replace").read()
t = re.sub(r"(?is)<(script|style|head)\b.*?</\1>", " ", raw)
t = re.sub(r"(?s)<[^>]+>", " ", t)
t = html.unescape(t)
t = t.replace("\u200b", "").replace("\ufeff", "")              # zero-width spaces
# normalize unicode punctuation/space to ASCII (en/em dash, curly quotes, nbsp)
t = (t.replace("\u2013", "-").replace("\u2014", "-")
      .replace("\u2018", "'").replace("\u2019", "'")
      .replace("\u201c", '"').replace("\u201d", '"')
      .replace("\u2026", "...").replace("\u00a0", " "))
t = re.sub(r"\S{40,}", " ", t)
t = re.sub(r"\s+", " ", t).strip()
# Part target 2600: each part becomes ONE chunk. The engine prepends the doc
# title (filename stem, ~22 chars) to every chunk AFTER the chunker bounds
# content at max_chars, and recipe test's size gate counts the prepended
# result — so a part must fit max_chars (3000) minus title headroom, or the
# gate is structurally red (see chunk_doc, engine/ingest_helpers.rs). A
# 300-char word-boundary overlap between parts preserves the recipe's
# claim-continuity intent across part cuts.
docs_dir, acc, target, overlap = sys.argv[2], sys.argv[3], 2600, 300
parts, cur, cap = [], [], 0
for tok in t.split(" "):
    if cap + len(tok) + 1 > target and cur:
        parts.append(" ".join(cur))
        tail, tlen = [], 0
        for w in reversed(cur):
            if tlen + len(w) + 1 > overlap:
                break
            tail.insert(0, w); tlen += len(w) + 1
        cur, cap = tail, tlen
    cur.append(tok); cap += len(tok) + 1
if cur:
    parts.append(" ".join(cur))
for i, p in enumerate(parts, 1):
    open(f"{docs_dir}/{acc}-{i:03d}.txt", "w", encoding="utf-8").write(p)
print(f"  cleaned -> {len(parts)} prose part file(s), {len(t)} chars total")
PY
[[ -n "$(ls "${PROSE_DIR}"/*.txt 2>/dev/null)" ]] || { echo "FATAL: skipped filing ${ACC}: cleaning produced no text" >&2; exit 1; }

# ── 4. Fetch companyfacts (XBRL figures, full history, typed) ────────────────
FACTS_JSON="${RAW_DIR}/companyfacts.json"
echo "  fetching companyfacts: CIK${CIK10}"
curl -s --max-time 120 -H "User-Agent: $CONTACT_UA" \
  "https://data.sec.gov/api/xbrl/companyfacts/CIK${CIK10}.json" -o "$FACTS_JSON"
jq -e '.facts' "$FACTS_JSON" >/dev/null 2>&1 || {
  echo "FATAL: companyfacts response for CIK${CIK10} is not a facts document" >&2; exit 1; }

# ── 5. Render figures via THE decider (concept map -> fact .txt files) ───────
# Debug trace names every alias fired; the unmapped-tag list lands as
# facts/_unmapped_concepts.json — a deliverable, not a log line.
# (debug log lives under raw/, keeping docs/ ingest-only)
python3 "$DECIDER" --debug render --map "$CONCEPT_MAP" --facts "$FACTS_JSON" \
  --out "$FACTS_DIR" ${TICKER:+--ticker "$TICKER"} \
  ${FY_ARGS[@]+"${FY_ARGS[@]}"} \
  2> "${RAW_DIR}/render_debug.log"
echo "  render debug trace (every alias fired): ${RAW_DIR}/render_debug.log"
# aux json deliverables live under raw/, keeping docs/ ingest-only .txt.
# sec_facts.json is the typed fact sidecar the Rust `sec_facts` tool answers
# from — installed into the corpus INDEX dir after `corpus install` below.
mv "${FACTS_DIR}/_unmapped_concepts.json" "${FACTS_DIR}/_render_manifest.json" \
   "${FACTS_DIR}/sec_facts.json" "${RAW_DIR}/"
UNMAPPED_N="$(jq '.unmapped | length' "${RAW_DIR}/_unmapped_concepts.json")"
TOTAL_N="$(jq '.filer_tags_total' "${RAW_DIR}/_unmapped_concepts.json")"
echo "  unmapped filer tags: ${UNMAPPED_N}/${TOTAL_N} (named in ${RAW_DIR}/_unmapped_concepts.json)"
[[ -n "$(ls "${FACTS_DIR}"/facts-*.txt 2>/dev/null)" ]] || { echo "FATAL: fact rendering produced no files" >&2; exit 1; }

# ── 6. Materialize the per-company recipe override ───────────────────────────
OVERRIDE_DIR="${HOME}/.svrnmesh/recipes/${CORPUS_ID}"
OVERRIDE_RECIPE="${OVERRIDE_DIR}/recipe.toml"
mkdir -p "$OVERRIDE_DIR"
SAFE_TITLE="$(printf '%s' "$TITLE" | sed 's/[&|\\]/ /g')"
SAFE_DESC="SEC filings for ${SAFE_TITLE}: Form 10-K prose (accession ${ACC}, filed ${FDATE}) plus XBRL companyfacts figures rendered with unit, fiscal period basis, and accession."
# The canonical recipe acquires BY TICKER through the `sec_edgar` custom
# acquirer. This script has already downloaded and rendered everything,
# so the override must acquire from disk instead — which means replacing
# the whole `[acquire]` block and dropping `[parameters]` (a required
# ticker on a materialized per-company recipe would demand an input the
# company is already pinned to). Both are whole-BLOCK edits, so this is a
# TOML-aware rewrite rather than the line-oriented sed it replaces: sed
# silently produced a recipe with no `path` the day `[acquire]` stopped
# being two lines, and a silently wrong recipe is worse than a refusal.
python3 - "$CANONICAL_RECIPE" "$OVERRIDE_RECIPE" "$CORPUS_ID" \
         "${SAFE_TITLE} — SEC Filings (10-K + facts)" "$SAFE_DESC" "$DOCS_DIR" <<'PY'
import sys
src, dst, corpus_id, name, desc, docs_dir = sys.argv[1:7]
lines = open(src, encoding="utf-8").read().splitlines()
DROP = ("[acquire]", "[parameters]", "[parameters.ticker]")

# A comment run belongs to whatever it HEADS, so it is buffered until the
# next real line claims it. Dropping a block therefore drops that block's
# own heading and nothing else's — an earlier version popped backwards
# off the output and swallowed the [authority] heading, which is the one
# block whose comment explains why the tool may claim this corpus.
out, pending, skip_block = [], [], False
for line in lines:
    stripped = line.strip()
    if stripped.startswith("#") or not stripped:
        pending.append(line)
        continue
    if stripped.startswith("[") and not stripped.startswith("[["):
        if stripped in DROP:
            pending = []
            skip_block = True
            if stripped == "[acquire]":
                out += ["",
                        "# Materialized by setup-sec-corpus.sh: the bytes are already on",
                        "# disk, so this override reads them rather than re-acquiring.",
                        "[acquire]",
                        'type = "local_file"',
                        f'path = "{docs_dir}"']
            continue
        skip_block = False
        out += pending; pending = []
        out.append(line)
        continue
    if skip_block:
        continue
    out += pending; pending = []
    if line.startswith('id = "sec-filings-company"'):
        out.append(f'id = "{corpus_id}"')
    elif line.startswith("name = "):
        out.append(f'name = "{name}"')
    elif line.startswith("description = "):
        out.append(f'description = "{desc}"')
    elif line.startswith("on_demand = "):
        out.append("on_demand = false")
    else:
        out.append(line)
out += pending
text = "\n".join(out) + "\n"
# Refuse rather than emit a recipe that cannot install: a silently
# malformed override is exactly the failure this rewrite exists to stop.
for required in (f'id = "{corpus_id}"', 'type = "local_file"', f'path = "{docs_dir}"'):
    if required not in text:
        sys.exit(f"FATAL: materialized recipe is missing `{required}` — refusing to write it")
# Declarations only — a comment that MENTIONS [parameters] is prose, not
# a demand for user input, and failing on it would be a guard asserting
# on something the subject merely echoes back.
if any(l.lstrip().startswith("[parameters") for l in text.splitlines()):
    sys.exit("FATAL: materialized recipe still declares [parameters] — it would demand "
             "an input this per-company recipe has already pinned")
if any(l.lstrip().startswith('type = "custom"') for l in text.splitlines()):
    sys.exit("FATAL: materialized recipe still names the by-ticker acquirer — it would "
             "re-download what this script already fetched")
# The authority declaration is what lets the sec_facts tool claim this
# corpus at all (it discovers by declaration + sidecar, never by id).
# Losing it to a block edit would produce a corpus that installs green
# and then cannot answer a single figure.
if "[authority]" not in text:
    sys.exit("FATAL: materialized recipe lost its [authority] block — the sec_facts "
             "tool could not claim the corpus this builds")
open(dst, "w", encoding="utf-8").write(text)
PY
echo "materialized recipe: $OVERRIDE_RECIPE"
echo "  $(grep '^id = ' "$OVERRIDE_RECIPE")"
echo "  $(grep '^path = ' "$OVERRIDE_RECIPE")"

# ── 7. Install ───────────────────────────────────────────────────────────────
if [[ -n "$SKIP_INSTALL" ]]; then
  echo
  echo "── --skip-install: download + render + materialize only ──"
  echo "Iterate extraction (no model needed):"
  echo "    $BIN recipe test \"$OVERRIDE_RECIPE\" --no-embed --offline"
  echo "Ask THE decider directly (figures + refusals):"
  echo "    python3 $DECIDER ask --map $CONCEPT_MAP --facts $FACTS_JSON --concept revenue --period FY2025"
  echo "Then install when ready:  $BIN corpus install $CORPUS_ID"
  echo "And place the typed fact sidecar (the Rust sec_facts tool reads it):"
  echo "    cp ${RAW_DIR}/sec_facts.json ~/.svrnmesh/indexes/${CORPUS_ID}/sec_facts.json"
  exit 0
fi

echo "installing corpus '${CORPUS_ID}' …"
"$BIN" corpus install "$CORPUS_ID"

# Typed fact sidecar -> index dir: the Rust `sec_facts` tool resolves the
# store at <index_dir>/<corpus_id>/sec_facts.json (FINANCIAL_CORPORA §6.2).
cp "${RAW_DIR}/sec_facts.json" "${HOME}/.svrnmesh/indexes/${CORPUS_ID}/sec_facts.json"
echo "  typed fact sidecar installed: ~/.svrnmesh/indexes/${CORPUS_ID}/sec_facts.json"

echo
echo "✓ SEC filings corpus ready: ~/.svrnmesh/indexes/${CORPUS_ID}"
echo "Verify a figure travels with its basis:"
echo "    $BIN knowledge ask $CORPUS_ID \"what was research and development expense in fiscal 2025, and what did management say about it?\""
