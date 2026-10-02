#!/usr/bin/env bash
# acceptance.sh — prove the install on THEIR hardware, before a lawyer logs in.
#
# The proof runs here, at install time, against the running box: the
# on-prem binary's own tests run in our CI on a model-free engine, and
# nothing there knows this box's models, corpus, scanner or nginx.
#
#   DAEMON_URL=http://127.0.0.1:9741 API_KEY=<firm key> ADMIN_KEY=<it key> ./acceptance.sh
#   BASE_URL=https://firm-rag.example.com ... ./acceptance.sh
#
# ── Two front doors, both probed ─────────────────────────────────────
# DAEMON_URL is the daemon's own port. Checks 0 and 0b run against it
# ALWAYS: a refusal that only nginx makes is one config edit from gone.
# BASE_URL, when set, is the TLS front door; checks 0 and 0b run there
# too, and every lawyer-facing check (0c, 2, 3, 4) goes through it, so
# the allowlist is proven to let the product through. Unset, those run
# against DAEMON_URL and the nginx leg is reported as never run.
#
# ── Two keys ─────────────────────────────────────────────────────────
# API_KEY is a lawyer's key (install.sh's 'firm'); every lawyer-facing
# check uses it. ADMIN_KEY is IT's ('it', group admin): checks 1 and 5
# read the daemon's admin surface with it, and check 0 uses it to prove
# the withheld surfaces are absent even for IT.
#
# ── Four verdicts, not two ───────────────────────────────────────────
#   pass   the assertion ran and held
#   FAIL   the assertion ran and did not hold                → exit 1
#   UNSURE the assertion could not be evaluated (missing
#          probe, unreachable service, absent tool)          → exit 2
#   ----   never ran (script aborted earlier)
#
# UNSURE is NOT a pass and does not exit 0. An install where two checks
# could not be judged is an install with two unknowns, and reporting that
# as green is the exact failure this file exists to prevent. Gate on the
# EXIT CODE, never on reading the summary.

set -uo pipefail

# ── Configuration ────────────────────────────────────────────────────
# Probe values that depend on the FIRM (their practice area, their
# scanned document) live in a separate file the installer fills in. They
# are deliberately not defaulted: a golden question invented by us tests
# our imagination, not their corpus.
PROBES_FILE="${PROBES_FILE:-/etc/firm-rag/acceptance-probes.env}"
# The caller's environment wins over the file: install.sh passes the
# keys and URLs it just wrote, and the template's blanks must not erase
# them.
declare -A CALLER=()
for v in DAEMON_URL BASE_URL API_KEY ADMIN_KEY DAEMON_CONFIG DATA_DIR; do CALLER[$v]="${!v-}"; done
# shellcheck disable=SC1090
[ -f "$PROBES_FILE" ] && . "$PROBES_FILE"
for v in "${!CALLER[@]}"; do [ -n "${CALLER[$v]}" ] && printf -v "$v" '%s' "${CALLER[$v]}"; done

DAEMON_URL="${DAEMON_URL:-http://127.0.0.1:9741}"
BASE_URL="${BASE_URL:-}"
API_KEY="${API_KEY:-}"
ADMIN_KEY="${ADMIN_KEY:-}"
DAEMON_CONFIG="${DAEMON_CONFIG:-/etc/firm-rag/daemon-config.toml}"
DATA_DIR="${DATA_DIR:-/var/lib/firm-rag}"

# Filled in by whoever installs, from the firm's own practice area:
#   GOLDEN_QUESTION      a question an installed corpus can answer
#   GOLDEN_EXPECT_CORPUS the corpus its citations must come from
#                        (default: the first corpus in the allow-list)
#   ABSTAIN_QUESTION     IN-DOMAIN but absent — see check 4
#   OCR_FIXTURE_PDF      a scanned PDF with no text layer
#   OCR_EXPECT_PHRASE    a phrase that appears in that scan's text
GOLDEN_QUESTION="${GOLDEN_QUESTION:-}"
GOLDEN_EXPECT_CORPUS="${GOLDEN_EXPECT_CORPUS:-}"
ABSTAIN_QUESTION="${ABSTAIN_QUESTION:-}"
OCR_FIXTURE_PDF="${OCR_FIXTURE_PDF:-}"
OCR_EXPECT_PHRASE="${OCR_EXPECT_PHRASE:-}"

# The routes a lawyer's laptop may reach through nginx: "<methods> <path>",
# `{}` one path segment. nginx/firm-rag.conf proxies exactly these and
# nothing more (pinned by quality/xtask/tests/
# onprem_kit_allowlist.rs), and check 0b exercises every one.
CLIENT_ROUTES=(
    "GET /health"
    "GET POST /v1/conversations"
    "GET /v1/conversations/search"
    "GET DELETE /v1/conversations/{}"
    "POST /v1/conversations/{}/messages"
    "GET /v1/conversations/{}/stream"
    "GET /v1/tools"
    "GET /v1/corpora"
    "GET /v1/corpora/{}/chunks/{}"
    "GET /v1/documents"
    "GET /v1/documents/{}"
    "GET /v1/documents/{}/progress"
    "POST /v1/documents/{}/ask"
    "GET /v1/documents/{}/ask/{}"
)

# ── Verdict bookkeeping ──────────────────────────────────────────────
declare -a NAMES=() VERDICTS=() DETAILS=()
FAILED=0
UNSURE=0

_record() { NAMES+=("$1"); VERDICTS+=("$2"); DETAILS+=("$3"); }
ok()     { _record "$1" pass   "$2"; printf '  \033[32mpass\033[0m   %s  %s\n' "$1" "$2"; }
bad()    { _record "$1" FAIL   "$2"; FAILED=$((FAILED+1)); printf '  \033[31mFAIL\033[0m   %s\n         %s\n' "$1" "$2"; }
unsure() { _record "$1" UNSURE "$2"; UNSURE=$((UNSURE+1)); printf '  \033[33mUNSURE\033[0m %s\n         %s\n' "$1" "$2"; }

# ── Preflight: refuse to run half-blind ──────────────────────────────
# A missing `jq` would otherwise degrade every JSON assertion into a grep
# that passes on the wrong thing. Absence is reported, never worked
# around.
for tool in curl jq; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "acceptance: \`$tool\` is not installed. Every JSON assertion below" >&2
        echo "            depends on it; running without it would report a green" >&2
        echo "            that means nothing. Install it and re-run." >&2
        exit 2
    }
done
for v in API_KEY ADMIN_KEY; do
    [ -n "${!v}" ] || {
        echo "acceptance: $v is unset. install.sh wrote both keys to" >&2
        echo "            /etc/firm-rag/issued-keys.txt ('firm' is API_KEY, 'it' ADMIN_KEY)." >&2
        exit 2
    }
done
DAEMON_URL="${DAEMON_URL%/}"
BASE_URL="${BASE_URL%/}"
# The lawyer-facing checks go through the front door when there is one.
FRONT="${BASE_URL:-$DAEMON_URL}"

# curl wrapper: prints "<body>\n<http_code>". --max-time is generous
# because a grounded turn on a 35B model is minutes, not seconds.
# `-k` only under ACCEPTANCE_INSECURE=1, for a self-signed sandbox cert.
req() {
    local base="$1" key="$2" method="$3" path="$4" body="${5:-}"
    local -a args=(-sS -o - -w '\n%{http_code}' --max-time 900 -X "$method" "$base$path")
    [ "${ACCEPTANCE_INSECURE:-0}" = 1 ] && args+=(-k)
    [ -n "$key" ] && args+=(-H "Authorization: Bearer $key")
    if [ -n "$body" ]; then
        args+=(-H 'Content-Type: application/json' --data "$body")
    fi
    curl "${args[@]}" 2>/dev/null
}
code_of() { printf '%s' "$1" | tail -n1; }
body_of() { printf '%s' "$1" | sed '$d'; }

# curl writes the literal `000` — not an empty string — when it never got
# an HTTP response at all (connection refused, DNS failure, TLS reject).
# Without this, "the service is down" reads as "the route answered with
# something that isn't a refusal". The verdict there has to be
# could-not-judge, never fail-or-pass.
no_response() { [ -z "$1" ] || [ "$1" = "000" ]; }

# The corpus allow-list install.sh wrote, one id per line.
allow_list() {
    awk '/^\[retrieval\]/{f=1;next} /^\[/{f=0} f && /^corpora *=/' "$DAEMON_CONFIG" 2>/dev/null \
        | grep -o '"[^"]*"' | tr -d '"'
}

echo
echo "acceptance: daemon $DAEMON_URL   front door ${BASE_URL:-(none — nginx leg never ran)}"
echo

FRONTS=("$DAEMON_URL")
[ -n "$BASE_URL" ] && FRONTS+=("$BASE_URL")

# ─────────────────────────────────────────────────────────────────────
# 0 — SECURITY: the shell, upload and MCP routes are never 2xx
#
# The on-prem binary does not compose code (no solve, no projects: a
# client-supplied command could reach a shell there) or svrn's MCP route;
# each answers a 503 that names the absence. Probed with IT's key, so the
# absence is proven for the most privileged caller, not just hidden from
# a lawyer. The ingest routes take an absolute SERVER-side path: a
# lawyer's key must be refused them (any key could otherwise read the
# config into a searchable corpus). Through nginx every one is a 404.
# ─────────────────────────────────────────────────────────────────────
WITHHELD=("POST /v1/solve/jobs" "POST /v1/projects" "POST /mcp" "POST /mcp/message" "GET /mcp/stats")
INGEST=("POST /v1/documents" "POST /v1/documents/legacy" "POST /internal/corpus/local" "POST /internal/corpus/watch/register")
for base in "${FRONTS[@]}"; do
    for probe in "${WITHHELD[@]}" "${INGEST[@]}"; do
        m="${probe%% *}"; p="${probe#* }"
        key="$API_KEY"
        for w in "${WITHHELD[@]}"; do [ "$w" = "$probe" ] && key="$ADMIN_KEY"; done
        who=lawyer; [ "$key" = "$ADMIN_KEY" ] && who=IT
        r="$(req "$base" "$key" "$m" "$p" '{"path":"/etc/hostname"}')"; c="$(code_of "$r")"
        name="0  $probe ($who) at $base"
        if no_response "$c"; then
            unsure "$name" "no HTTP response — service down, DNS failed, or TLS rejected. NOT a pass: nothing was proven about this route."
        elif [ "$c" = "401" ]; then
            unsure "$name" "401: the key was not accepted, so the route was never reached. Check $who's key."
        else
            case "$c" in
                2??) bad "$name" "REACHABLE: answered $c. The installed binary composes a surface it must not, or nginx proxies it." ;;
                403|404|503) ok "$name" "$c" ;;
                *) bad "$name" "unexpected $c: $(body_of "$r" | head -c 200)" ;;
            esac
        fi
    done
done

# ─────────────────────────────────────────────────────────────────────
# 0b — SECURITY: no key gets 401, on every client route
#
# Behind nginx every request arrives from loopback, so the daemon must
# identify every caller by key: a daemon with no key in its store serves
# loopback as the owner. Every route in CLIENT_ROUTES, with no key, must
# be 401 — except /health, which answers `ok` and nothing else.
# ─────────────────────────────────────────────────────────────────────
for base in "${FRONTS[@]}"; do
    for route in "${CLIENT_ROUTES[@]}"; do
        path="${route##* }"; methods="${route% *}"
        path="${path//\{\}/acceptance-probe}"
        for m in $methods; do
            r="$(req "$base" "" "$m" "$path")"; c="$(code_of "$r")"
            name="0b $m $path, no key, at $base"
            if no_response "$c"; then
                unsure "$name" "no HTTP response — cannot judge whether auth is on"
            elif [ "$path" = "/health" ]; then
                if [ "$c" = "200" ] && [ "$(body_of "$r")" = "ok" ]; then
                    ok "$name" "200 ok (the one unkeyed route)"
                else
                    bad "$name" "expected 200 'ok', got $c: $(body_of "$r" | head -c 120)"
                fi
            elif [ "$c" = "401" ]; then
                ok "$name" "401"
            else
                bad "$name" "SERVED WITHOUT A KEY: got $c, expected 401. The daemon holds no API key (\`svrn daemon key --list\`), or this is not the route nginx should proxy."
            fi
        done
    done
done

# ─────────────────────────────────────────────────────────────────────
# 0c — SECURITY: no web tool is held
#
# The agent tools that reach the open internet on an ordinary turn —
# `web_fetch`, `wikipedia_fetch`, recipe authoring's `probe_url` — are
# not composed in the on-prem binary. GET /v1/tools lists what the turn
# runtime actually holds. `search` MUST still be present: it is corpus
# search, which is the product; only its web fallback is withheld.
# ─────────────────────────────────────────────────────────────────────
r="$(req "$FRONT" "$API_KEY" GET /v1/tools)"; c="$(code_of "$r")"; b="$(body_of "$r")"
if no_response "$c"; then
    unsure "0c no web tool is held" "no HTTP response from $FRONT"
elif [ "$c" != "200" ]; then
    unsure "0c no web tool is held" "GET /v1/tools returned $c — cannot enumerate the registry"
else
    tool_ids="$(printf '%s' "$b" | jq -r '.tools[]?.id' 2>/dev/null)"
    if [ -z "$tool_ids" ]; then
        unsure "0c no web tool is held" "could not read tool ids out of GET /v1/tools: $(printf '%s' "$b" | head -c 200)"
    else
        leaked=""
        for t in web_fetch wikipedia_fetch probe_url; do
            printf '%s\n' "$tool_ids" | grep -qx "$t" && leaked="$leaked $t"
        done
        if [ -n "$leaked" ]; then
            bad "0c no web tool is held" "the turn runtime holds:$leaked. This is not the on-prem binary, or it composes web reach. These reach the open internet on ordinary chat turns."
        elif ! printf '%s\n' "$tool_ids" | grep -qx "search"; then
            bad "0c no web tool is held" "the web tools are gone, but so is \`search\` — corpus search is the product. Retrieval may be crippled."
        else
            ok "0c no web tool is held" "web_fetch/wikipedia_fetch/probe_url absent; search present"
        fi
    fi
fi

# ─────────────────────────────────────────────────────────────────────
# 1 — SLOTS: the models are resident
#
# Assert on `inference.resident[]`, NOT on `loaded_models`. The latter is
# plan-derived and joins on the registered MODEL NAME rather than the
# slot role, so it can report a name that is configured but not loaded.
#
# The primary loads on first use, so a box that has answered nothing yet
# reports it configured but not resident. This check therefore READS
# after checks 3 and 4 have asked their questions (it is defined here and
# called below them). `transitioning: true` means residency was
# indeterminate at read time (the slot lock was contended) — neither a
# pass nor a fail, it is "ask again", so it is reported as UNSURE.
# ─────────────────────────────────────────────────────────────────────
check_slots() {
    local st
    st="$(curl -sS --max-time 30 -H "Authorization: Bearer $ADMIN_KEY" "$DAEMON_URL/status" 2>/dev/null)"
    if [ -z "$st" ] || ! printf '%s' "$st" | jq -e . >/dev/null 2>&1; then
        unsure "1  model slots resident" "daemon at $DAEMON_URL returned no parseable /status"
        return
    fi
    for role in primary fast embed; do
        slot="$(printf '%s' "$st" | jq -c --arg r "$role" '.inference.resident[]? | select(.role == $r)' 2>/dev/null)"
        if [ -z "$slot" ]; then
            bad "1  slot '$role' resident" "no entry with role=\"$role\" in .inference.resident[] — the slot is not configured at all"
        elif [ "$(printf '%s' "$slot" | jq -r '.transitioning')" = "true" ]; then
            unsure "1  slot '$role' resident" "transitioning=true — residency indeterminate at read time; re-run"
        elif [ "$(printf '%s' "$slot" | jq -r '.resident')" = "true" ]; then
            ok "1  slot '$role' resident" "$(printf '%s' "$slot" | jq -r '.model_id')"
        else
            bad "1  slot '$role' resident" "resident=false (model_id=$(printf '%s' "$slot" | jq -r '.model_id')) after checks 3 and 4 asked it questions. With primary_idle_secs=86400 this should not idle-unload; check the daemon log for a load failure."
        fi
    done
}

# ─────────────────────────────────────────────────────────────────────
# 2 — CORPUS: every corpus in the allow-list is listed
#
# `GET /v1/corpora` lists the installed corpora the key's grant admits,
# and the grant is `[retrieval] corpora`. So this also proves the list
# names real, installed ids — a typo there is silent (no
# deny_unknown_fields) and would scope retrieval to nothing.
# ─────────────────────────────────────────────────────────────────────
mapfile -t ALLOWED < <(allow_list)
if [ "${#ALLOWED[@]}" -eq 0 ]; then
    bad "2  allow-listed corpora listed" "[retrieval] corpora in $DAEMON_CONFIG is empty or unreadable. On a keyed daemon empty grants NOTHING: every answer would be ungrounded."
else
    r="$(req "$FRONT" "$API_KEY" GET /v1/corpora)"; c="$(code_of "$r")"; b="$(body_of "$r")"
    if no_response "$c"; then
        unsure "2  allow-listed corpora listed" "no HTTP response from $FRONT"
    elif [ "$c" != "200" ]; then
        bad "2  allow-listed corpora listed" "GET /v1/corpora returned $c"
    else
        listed="$(printf '%s' "$b" | jq -r '.corpora[]? | .corpus_id // .id' 2>/dev/null)"
        missing=""
        for id in "${ALLOWED[@]}"; do
            printf '%s\n' "$listed" | grep -qxF "$id" || missing="$missing $id"
        done
        if [ -n "$missing" ]; then
            bad "2  allow-listed corpora listed" "not listed:$missing. Either it is not installed (the restore did not land, the share is not registered), or [retrieval] corpora misspells it."
        else
            ok "2  allow-listed corpora listed" "${ALLOWED[*]}"
        fi
    fi
fi
[ -n "$GOLDEN_EXPECT_CORPUS" ] || GOLDEN_EXPECT_CORPUS="${ALLOWED[0]:-}"

# ── Helper: run one turn, echo the assistant MessageResponse ─────────
ask() {
    local question="$1"
    local conv cid payload resp
    conv="$(req "$FRONT" "$API_KEY" POST /v1/conversations '{}')"
    case "$(code_of "$conv")" in 200|201) ;; *) printf ''; return 1 ;; esac
    cid="$(body_of "$conv" | jq -r '.id // .conversation_id // empty')"
    [ -n "$cid" ] || { printf ''; return 1; }
    payload="$(jq -nc --arg c "$question" '{content: $c}')"
    resp="$(req "$FRONT" "$API_KEY" POST "/v1/conversations/$cid/messages" "$payload")"
    [ "$(code_of "$resp")" = "200" ] || { printf ''; return 1; }
    body_of "$resp"
}

# ─────────────────────────────────────────────────────────────────────
# 3 — GROUNDED ANSWER: citations carry a real (corpus_id, chunk_id)
#
# `citations` is `skip_serializing_if = "Vec::is_empty"`, so an ungrounded
# answer OMITS the key rather than sending []. Absence and emptiness are
# the same failure and both must be caught.
# ─────────────────────────────────────────────────────────────────────
if [ -z "$GOLDEN_QUESTION" ]; then
    unsure "3  grounded answer has citations" "GOLDEN_QUESTION is unset in $PROBES_FILE. It must come from the firm's own practice area — a question we invent tests our imagination, not their corpus."
else
    msg="$(ask "$GOLDEN_QUESTION")"
    if [ -z "$msg" ]; then
        unsure "3  grounded answer has citations" "the turn did not complete (see the daemon journal)"
    else
        n="$(printf '%s' "$msg" | jq '(.citations // []) | length')"
        if [ "${n:-0}" -eq 0 ]; then
            bad "3  grounded answer has citations" "citations absent or empty — the answer was not grounded in an installed corpus"
        elif ! printf '%s' "$msg" | jq -e --arg cid "$GOLDEN_EXPECT_CORPUS" \
                '.citations | map(select(.corpus_id == $cid and (.chunk_id | length) > 0)) | length > 0' >/dev/null 2>&1; then
            bad "3  grounded answer has citations" "got $n citation(s) but none from corpus '$GOLDEN_EXPECT_CORPUS' with a non-empty chunk_id. Sources: $(printf '%s' "$msg" | jq -c '[.citations[].corpus_id] | unique')"
        else
            ok "3  grounded answer has citations" "$n citation(s), incl. $GOLDEN_EXPECT_CORPUS"
        fi
    fi
fi

# ─────────────────────────────────────────────────────────────────────
# 4 — ABSTENTION: the box refuses to answer what it cannot source
#
# This is the check the whole pilot is for. Two traps:
#
#   * The raw `grounding_gate.action` is NOT projected onto the wire, so
#     `epistemic_state.verdict` is the only structured handle. Like
#     `citations`, it is skipped when None — absent means "no ledger was
#     stamped", which is a fail, not a pass.
#
#   * The probe must be IN-DOMAIN BUT ABSENT. An out-of-domain question
#     triggers gk_rescue, which replaces the abstention with a caveated
#     parametric answer and rewrites the action to gk_rescue_released.
#     That makes this check flap between runs, which is worse than not
#     having it. Build the probe like the chaos-monkey banks: a fact
#     whose absence from the corpus can be CERTIFIED.
# ─────────────────────────────────────────────────────────────────────
if [ -z "$ABSTAIN_QUESTION" ]; then
    unsure "4  abstains on the unsourceable" "ABSTAIN_QUESTION is unset in $PROBES_FILE. It must be in-domain but absent, and its absence must be certifiable — an out-of-domain probe silently trips gk_rescue and makes this check flap."
else
    msg="$(ask "$ABSTAIN_QUESTION")"
    if [ -z "$msg" ]; then
        unsure "4  abstains on the unsourceable" "the turn did not complete (see the daemon journal)"
    else
        v="$(printf '%s' "$msg" | jq -r '.epistemic_state.verdict // "ABSENT"')"
        case "$v" in
            cannot_know_from_here)
                ok "4  abstains on the unsourceable" "verdict=cannot_know_from_here" ;;
            ABSENT)
                bad "4  abstains on the unsourceable" "no epistemic_state on the response at all. The turn stamped no ledger — check that the grounding gate ran (a zero-chunk turn takes the retrieval-miss path and produces no gate metadata)." ;;
            general_knowledge|mixed)
                bad "4  abstains on the unsourceable" "verdict=$v — the box answered from parametric knowledge instead of abstaining. If the probe is out-of-domain it tripped gk_rescue; make it in-domain-but-absent. Answer: $(printf '%s' "$msg" | jq -r '.content' | head -c 200)" ;;
            *)
                bad "4  abstains on the unsourceable" "verdict=$v, expected cannot_know_from_here. Answer: $(printf '%s' "$msg" | jq -r '.content' | head -c 200)" ;;
        esac
    fi
fi

# Residency is read after the turns: the primary loads on first use.
check_slots

# ─────────────────────────────────────────────────────────────────────
# 5 — OCR: a scanned PDF produces searchable text
#
# For a litigation practice, scanned PDFs are not an edge case — they are
# the corpus. Through the daemon's own API with IT's key: register a
# throwaway watched folder with OCR on, wait for its sweep, then:
#
#   (a) NEGATIVE: the file is not in the sweep's failed_files. A
#       `scanned_no_text` reason there is what you get when the binary
#       was built without --features ocr, or built with it and could not
#       resolve its models.
#   (b) POSITIVE: a phrase known to be in the scan comes back from a
#       search of that corpus. (a) alone only proves nothing complained.
#
# The folder lives under the daemon's data dir, the one place its
# systemd sandbox (ProtectSystem=strict, PrivateTmp) lets it read a file
# this script wrote. It is removed afterwards.
# ─────────────────────────────────────────────────────────────────────
if [ -z "$OCR_FIXTURE_PDF" ] || [ -z "$OCR_EXPECT_PHRASE" ]; then
    unsure "5  OCR reads a scanned PDF" "OCR_FIXTURE_PDF / OCR_EXPECT_PHRASE unset in $PROBES_FILE. Use one of the firm's own scans — our test images say nothing about their scanner, their DPI, or their paper."
elif [ ! -f "$OCR_FIXTURE_PDF" ]; then
    unsure "5  OCR reads a scanned PDF" "fixture not found at $OCR_FIXTURE_PDF"
elif [ ! -d "$DATA_DIR" ]; then
    unsure "5  OCR reads a scanned PDF" "no data dir at $DATA_DIR; set DATA_DIR"
else
    ocr_dir="$DATA_DIR/acceptance-ocr-$$"
    mkdir -p "$ocr_dir" && cp "$OCR_FIXTURE_PDF" "$ocr_dir/" && chmod -R a+rX "$ocr_dir"
    body="$(jq -nc --arg p "$ocr_dir" '{path: $p, display_name: "acceptance-ocr", config: {with_ocr: true}, sync_initial: true}')"
    r="$(req "$DAEMON_URL" "$ADMIN_KEY" POST /internal/corpus/watch/register "$body")"
    ocr_cid="$(body_of "$r" | jq -r '.corpus_id // empty' 2>/dev/null)"
    if [ -z "$ocr_cid" ]; then
        unsure "5  OCR reads a scanned PDF" "the daemon did not register the OCR folder ($(code_of "$r")): $(body_of "$r" | head -c 300)"
    else
        # The register call returns before OCR finishes: poll the sweep.
        state=""
        for _ in $(seq 1 150); do
            state="$(body_of "$(req "$DAEMON_URL" "$ADMIN_KEY" GET "/internal/corpus/watch/state/$ocr_cid")")"
            [ "$(printf '%s' "$state" | jq '(.live_entries // 0) + (.failed_files // [] | length)' 2>/dev/null)" -gt 0 ] 2>/dev/null && break
            sleep 2
        done
        if printf '%s' "$state" | jq -e '.failed_files | length > 0' >/dev/null 2>&1; then
            bad "5  OCR reads a scanned PDF" "the scan landed in failed_files: $(printf '%s' "$state" | jq -c '.failed_files' | head -c 300). scanned_no_text means the binary was not built --features ocr, or could not resolve the PaddleOCR models / libpdfium — the journal's 'ocr:unavailable' line lists every path it probed."
        elif [ "$(printf '%s' "$state" | jq '.live_entries // 0' 2>/dev/null)" -eq 0 ] 2>/dev/null; then
            unsure "5  OCR reads a scanned PDF" "the sweep indexed nothing and reported no failure within 5 minutes: $(printf '%s' "$state" | head -c 300)"
        else
            q="$(jq -nc --arg q "$OCR_EXPECT_PHRASE" '{query: $q, limit: 5}')"
            hit="$(body_of "$(req "$DAEMON_URL" "$ADMIN_KEY" POST "/internal/corpus/local/$ocr_cid/search" "$q")")"
            if printf '%s' "$hit" | jq -r '.[]?.content' 2>/dev/null | grep -qiF "$OCR_EXPECT_PHRASE"; then
                if printf '%s' "$hit" | grep -qF 'cleanup unavailable'; then
                    ok "5  OCR reads a scanned PDF" "extracted text is searchable (RAW OCR: the language-model cleanup did not run — $(printf '%s' "$hit" | grep -o 'cleanup unavailable[^)]*' | head -n1))"
                else
                    ok "5  OCR reads a scanned PDF" "extracted text is searchable"
                fi
            else
                bad "5  OCR reads a scanned PDF" "the file was indexed, but '$OCR_EXPECT_PHRASE' does not come back from search. OCR likely produced empty or garbled text. Top hit: $(printf '%s' "$hit" | jq -r '.[0].content // empty' 2>/dev/null | head -c 200)"
            fi
        fi
        req "$DAEMON_URL" "$ADMIN_KEY" DELETE "/internal/corpus/watch/$ocr_cid" >/dev/null || true
    fi
    rm -rf "$ocr_dir"
fi

# ── Summary ──────────────────────────────────────────────────────────
echo
printf '%s\n' "─────────────────────────────────────────────────────────"
total=${#NAMES[@]}
passed=0
for v in "${VERDICTS[@]}"; do [ "$v" = "pass" ] && passed=$((passed+1)); done
printf 'acceptance: %d checks — %d pass, %d FAIL, %d UNSURE\n' \
    "$total" "$passed" "$FAILED" "$UNSURE"
[ -n "$BASE_URL" ] || echo "acceptance: the nginx leg NEVER RAN (BASE_URL unset) — owed before sign-off"

if [ "$FAILED" -gt 0 ]; then
    echo
    echo "This install is NOT ready. Failing checks:"
    for i in "${!NAMES[@]}"; do
        [ "${VERDICTS[$i]}" = "FAIL" ] && printf '  · %s\n    %s\n' "${NAMES[$i]}" "${DETAILS[$i]}"
    done
    exit 1
fi

if [ "$UNSURE" -gt 0 ]; then
    echo
    echo "Nothing failed, but $UNSURE check(s) could not be judged — which is"
    echo "not the same as passing. Resolve these before sign-off:"
    for i in "${!NAMES[@]}"; do
        [ "${VERDICTS[$i]}" = "UNSURE" ] && printf '  · %s\n    %s\n' "${NAMES[$i]}" "${DETAILS[$i]}"
    done
    exit 2
fi

echo
echo "All checks passed. The box refuses what it cannot source, and the"
echo "routes that could reach a shell are not in it."
exit 0
