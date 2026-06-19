#!/usr/bin/env bash
#
# setup-proxy-corpus.sh — one-command, reproducible setup of a single-issuer
# Proxy Voting Corpus from SEC EDGAR, installed under the machine-stable
# corpus_id `proxy-cik<10-digit-CIK>` (FR-1/FR-2).
#
# Pipeline (all EDGAR-specific resolution lives here, exactly where the
# ticker -> CIK step does — mirroring scripts/setup-governance-corpus.sh):
#   1. resolve   — ticker -> CIK via the cached company_tickers.json.
#   2. discover  — the issuer's latest DEF 14A in the date window, via the
#                  data.sec.gov submissions API. jq joins its parallel arrays
#                  (form[]/accessionNumber[]/primaryDocument[]/filingDate[])
#                  trivially — the join EFTS's JSONPath surface cannot do — and
#                  hands back the TRUE primary-document filename.
#   3. fetch     — synthesize the Archives URL and download the DEF 14A HTML:
#                  https://www.sec.gov/Archives/edgar/data/<cik>/<acc_nodash>/<doc>
#                  (EDGAR's archives path wants the CIK with leading zeros
#                  stripped; the corpus_id keeps the 10-digit padded form).
#   4. materialize — write a per-issuer recipe override at
#                  ~/.sovereign/recipes/<corpus_id>/recipe.toml from the committed
#                  template (id/name/description/acquire.path rewritten,
#                  on_demand flipped off). The running daemon resolves the
#                  override dir first, so no rebuild/restart is needed.
#   5. install   — corpus install <corpus_id> (extract + chunk + embed + index).
#
# Why not the http_api acquirer / recipe-native EFTS discovery: EFTS hits carry
# no ready document URL (only `_id = accession:filename`, `adsh`, `ciks`), and
# http_api JSON-parses every response so it cannot fetch the HTML proxy doc
# except via `follow`, which extracts verbatim URLs EFTS does not provide. The
# URL must be synthesized; doing it here with jq is the least-code path and keeps
# the engine generic. (A recipe-native follow-URL template is possible future
# work; see the plan.)
#
# Prerequisites: `jq`, `curl`, and — for the install step — a reachable daemon
# with the embed model loaded. Use --skip-install to stop after download +
# materialize (lets you iterate extraction with `recipe test`, no model needed).
#
# Usage:
#   scripts/setup-proxy-corpus.sh <TICKER|CIK> [--bin <cli>] [--from YYYY-MM-DD]
#                                 [--to YYYY-MM-DD] [--skip-install]

set -euo pipefail

CONTACT_UA="commonwealth-ai/0.1 (proxy-voting-corpus; alexbryan01@gmail.com)"
BIN="${SOVEREIGN_CLI:-target/debug/sovereign-cli-llm}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CANONICAL_RECIPE="${REPO_ROOT}/sovereign-recipes/proxy-company/recipe.toml"
CACHE_DIR="${HOME}/.sovereign/cache/sec"
TICKERS_JSON="${CACHE_DIR}/company_tickers.json"
FROM_DATE="2024-01-01"
TO_DATE="2026-12-31"
ACCESSION=""
SKIP_INSTALL=""
ARG=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --from) FROM_DATE="$2"; shift 2 ;;
    --to) TO_DATE="$2"; shift 2 ;;
    --accession) ACCESSION="$2"; shift 2 ;;
    --skip-install) SKIP_INSTALL=1; shift ;;
    -h|--help) sed -n '2,53p' "$0"; exit 0 ;;
    -*) echo "unknown flag: $1" >&2; exit 2 ;;
    *) ARG="$1"; shift ;;
  esac
done

command -v jq      >/dev/null 2>&1 || { echo "FATAL: jq is required" >&2; exit 2; }
command -v curl    >/dev/null 2>&1 || { echo "FATAL: curl is required" >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || { echo "FATAL: python3 is required (HTML->text clean)" >&2; exit 2; }
[[ -n "$ARG" ]] || { echo "FATAL: pass a ticker (e.g. XOM) or a CIK" >&2; exit 2; }
[[ -f "$CANONICAL_RECIPE" ]] || { echo "FATAL: template recipe not found at $CANONICAL_RECIPE" >&2; exit 2; }

mkdir -p "$CACHE_DIR"

# ── 1. Resolve ticker -> CIK (numeric arg is taken as a CIK as-is) ───────────
if [[ "$ARG" =~ ^[0-9]+$ ]]; then
  CIK_RAW="$ARG"
  TITLE="CIK ${ARG}"
else
  if [[ ! -s "$TICKERS_JSON" ]]; then
    echo "fetching SEC company_tickers.json (cached at $TICKERS_JSON) …"
    curl -s --max-time 60 -H "User-Agent: $CONTACT_UA" \
      "https://www.sec.gov/files/company_tickers.json" -o "$TICKERS_JSON"
  fi
  TUP="$(jq -r --arg t "$(printf '%s' "$ARG" | tr '[:lower:]' '[:upper:]')" '
    [ to_entries[] | select(.value.ticker == $t) | .value ][0]
    | if . == null then empty else "\(.cik_str)\t\(.title)" end' "$TICKERS_JSON")"
  [[ -n "$TUP" ]] || { echo "FATAL: ticker '$ARG' not found in company_tickers.json" >&2; exit 1; }
  IFS=$'\t' read -r CIK_RAW TITLE <<<"$TUP"
fi

CIK10="$(printf '%010d' "$((10#$CIK_RAW))")"   # zero-padded → corpus_id + submissions API
CIK_BARE="$((10#$CIK_RAW))"                     # leading zeros stripped → archives path
CORPUS_ID="proxy-cik${CIK10}"
echo "resolved: ${ARG} → ${TITLE} (CIK ${CIK10}) → corpus_id ${CORPUS_ID}"

# ── 2. Discover the DEF 14A via the submissions API ──────────────────────────
# Default: the latest DEF 14A in [from, to]. With --accession, pin that exact
# filing (the reproducible-fixture path, NFR-2) regardless of the date window.
if [[ -n "$ACCESSION" ]]; then
  echo "discovering DEF 14A by pinned accession ${ACCESSION} …"
else
  echo "discovering latest DEF 14A in [${FROM_DATE} .. ${TO_DATE}] …"
fi
SUBS="$(curl -s --max-time 60 -H "User-Agent: $CONTACT_UA" \
  "https://data.sec.gov/submissions/CIK${CIK10}.json")"
HIT="$(printf '%s' "$SUBS" | jq -r --arg from "$FROM_DATE" --arg to "$TO_DATE" --arg acc "$ACCESSION" '
  .filings.recent as $r
  | [ range(0; ($r.form | length))
      | select($r.form[.] == "DEF 14A"
               and ( if $acc != "" then $r.accessionNumber[.] == $acc
                     else ($r.filingDate[.] >= $from and $r.filingDate[.] <= $to) end ))
      | {acc: $r.accessionNumber[.], doc: $r.primaryDocument[.], date: $r.filingDate[.]} ]
  | sort_by(.date) | last
  | if . == null then empty else "\(.acc)\t\(.doc)\t\(.date)" end')"
[[ -n "$HIT" ]] || {
  if [[ -n "$ACCESSION" ]]; then
    echo "FATAL: no DEF 14A with accession ${ACCESSION} for CIK ${CIK10} in recent filings." >&2
  else
    echo "FATAL: no DEF 14A for CIK ${CIK10} in [${FROM_DATE} .. ${TO_DATE}]." >&2
    echo "       Widen the window with --from/--to (e.g. fall-cycle filers)." >&2
  fi
  exit 1
}
IFS=$'\t' read -r ACC DOC FDATE <<<"$HIT"
ACC_NODASH="${ACC//-/}"
DOC_URL="https://www.sec.gov/Archives/edgar/data/${CIK_BARE}/${ACC_NODASH}/${DOC}"
echo "  latest DEF 14A: accession ${ACC}  filed ${FDATE}"
echo "  primary document: ${DOC}"

# ── 3. Fetch the primary document, then pre-clean HTML -> plain text ─────────
# Raw HTML is kept under raw/ for provenance; the engine ingests only the
# cleaned .txt under docs/ (plaintext extractor recurses for .txt, ignoring
# .html). Cleaning strips tags, unescapes entities, drops runs of >=50 non-space
# chars (URLs / dash-rules / inline-XBRL ids that defeat chunk splitting and FTS
# and pollute the atlas), and collapses whitespace.
PROXY_DIR="${HOME}/.sovereign/cache/proxy/${CORPUS_ID}"
RAW_DIR="${PROXY_DIR}/raw"
DOCS_DIR="${PROXY_DIR}/docs"
mkdir -p "$RAW_DIR" "$DOCS_DIR"
# Remove any stale artifacts from earlier recipe revisions so the corpus
# reflects only this run (e.g. .html left in docs/ before the plaintext switch,
# or part files from a prior cleaning).
rm -f "${DOCS_DIR}"/*.html "${DOCS_DIR}"/*.txt 2>/dev/null || true
RAW="${RAW_DIR}/${ACC_NODASH}.html"
if [[ -s "$RAW" ]]; then
  echo "  cached raw: $RAW"
else
  echo "  fetching: $DOC_URL"
  curl -s --max-time 120 -H "User-Agent: $CONTACT_UA" "$DOC_URL" -o "$RAW"
  [[ -s "$RAW" ]] || { echo "FATAL: download produced an empty file" >&2; exit 1; }
fi
echo "  raw bytes: $(wc -c < "$RAW" | tr -d ' ')"
python3 - "$RAW" "$DOCS_DIR" "$ACC_NODASH" <<'PY'
import sys, re, html
raw = open(sys.argv[1], encoding="utf-8", errors="replace").read()
t = re.sub(r"(?is)<(script|style|head)\b.*?</\1>", " ", raw)   # drop non-content
t = re.sub(r"(?s)<[^>]+>", " ", t)                              # strip tags
t = html.unescape(t)
t = t.replace("\u200b", "").replace("\ufeff", "")              # zero-width spaces (AMZN)
# normalize unicode punctuation/space to ASCII (en/em dash, curly quotes, nbsp)
t = (t.replace("\u2013", "-").replace("\u2014", "-")
      .replace("\u2018", "'").replace("\u2019", "'")
      .replace("\u201c", '"').replace("\u201d", '"')
      .replace("\u2026", "...").replace("\u00a0", " "))
t = re.sub(r"\S{40,}", " ", t)                                 # drop URL / dash-rule / XBRL-id runs
t = re.sub(r"\s+", " ", t).strip()                            # collapse whitespace
# Split into ~25k-char part files at word boundaries. Each part file becomes one
# atlas chapter (~6k tokens), safely under the model context window \u2014 the atlas
# extracts per-chapter, so a single whole-filing chapter overflows the context.
# Retrieval chunking is per-file, so the 3000-char chunks are unaffected.
docs_dir, acc, target = sys.argv[2], sys.argv[3], 25000
parts, cur, cap = [], [], 0
for tok in t.split(" "):
    if cap + len(tok) + 1 > target and cur:
        parts.append(" ".join(cur)); cur, cap = [], 0
    cur.append(tok); cap += len(tok) + 1
if cur:
    parts.append(" ".join(cur))
for i, p in enumerate(parts, 1):
    open(f"{docs_dir}/{acc}-{i:03d}.txt", "w", encoding="utf-8").write(p)
print(f"  cleaned -> {len(parts)} part file(s) (~{target} chars each), {len(t)} chars total")
PY
[[ -n "$(ls "${DOCS_DIR}"/*.txt 2>/dev/null)" ]] || { echo "FATAL: cleaning produced no text files" >&2; exit 1; }

# ── 4. Materialize the per-issuer recipe override ────────────────────────────
# Rewrite only the [corpus] id/name/description, on_demand, and acquire.path.
# Patterns are anchored to the template's literal values so they never touch the
# [[extract.sections]] name/description keys (which sit at column 0 too).
OVERRIDE_DIR="${HOME}/.sovereign/recipes/${CORPUS_ID}"
OVERRIDE_RECIPE="${OVERRIDE_DIR}/recipe.toml"
mkdir -p "$OVERRIDE_DIR"
SAFE_TITLE="$(printf '%s' "$TITLE" | sed 's/[&|\\]/ /g')"
SAFE_DESC="Proxy statement (SEC DEF 14A, filed ${FDATE}, accession ${ACC}) for ${SAFE_TITLE}. The shareholder ballot and the sides for each item."
sed \
  -e "s|^id = \"proxy-company\"|id = \"${CORPUS_ID}\"|" \
  -e "s|^name = \"Proxy Statement.*|name = \"${SAFE_TITLE} — Proxy (DEF 14A)\"|" \
  -e "s|^description = \"Template for.*|description = \"${SAFE_DESC}\"|" \
  -e "s|^on_demand = true|on_demand = false|" \
  -e "s|^path = .*|path = \"${DOCS_DIR}\"|" \
  "$CANONICAL_RECIPE" > "$OVERRIDE_RECIPE"
echo "materialized recipe: $OVERRIDE_RECIPE"
echo "  $(grep '^id = ' "$OVERRIDE_RECIPE")"
echo "  $(grep '^path = ' "$OVERRIDE_RECIPE")"

# ── 5. Install ───────────────────────────────────────────────────────────────
if [[ -n "$SKIP_INSTALL" ]]; then
  echo
  echo "── --skip-install: download + materialize only ──"
  echo "Iterate extraction (no model needed):"
  echo "    $BIN recipe test \"$OVERRIDE_RECIPE\" --no-embed --offline"
  echo "    cat \"${DOCS_DIR}/_section_misses.json\"   # critical-miss diagnostics (AC-2)"
  echo "Then install when ready:  $BIN corpus install $CORPUS_ID"
  exit 0
fi

echo "installing corpus '${CORPUS_ID}' …"
"$BIN" corpus install "$CORPUS_ID"

echo
echo "✓ proxy corpus ready: ~/.sovereign/indexes/${CORPUS_ID}"
echo
echo "Verify the one-pager (RL-2 — both sides, cited):"
echo "    $BIN knowledge ask $CORPUS_ID \"for each proposal, what is being voted on and what are the sides?\""
echo "Honesty (RL-1 — no manufactured opposition on a management item):"
echo "    $BIN knowledge ask $CORPUS_ID \"what is the case against ratifying the auditors?\""
echo
echo "Next (Inc 2 — the sides as typed atoms):"
echo "    $BIN enrich init  $CORPUS_ID --from-corpus $CORPUS_ID"
echo "    $BIN enrich build $CORPUS_ID --full"
