#!/usr/bin/env bash
# acceptance.sh — prove the install on THEIR hardware, before a lawyer logs in.
#
# This is the compensating control for a real gap: sovereign-server's
# routes.rs / ws.rs / auth.rs / tenant.rs carry no tests, there is no
# release job for that binary, and no contract journey drives it. Nothing
# in CI has ever exercised this configuration. So the proof runs here, at
# install time, against the running box.
#
#   ./acceptance.sh                      # against https://<hostname> from probes file
#   BASE_URL=https://firm-rag.example.com ./acceptance.sh
#
# ── Point it at nginx, not at the backend ────────────────────────────
# BASE_URL should be the TLS front door. Testing 127.0.0.1:8080 directly
# bypasses the route allowlist and proves nothing about what a laptop on
# the firm's network can reach. Check 0 is meaningless against the
# backend port.
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
# shellcheck disable=SC1090
[ -f "$PROBES_FILE" ] && . "$PROBES_FILE"

BASE_URL="${BASE_URL:-}"
API_KEY="${API_KEY:-}"
DAEMON_URL="${DAEMON_URL:-http://127.0.0.1:9741}"
EXPECTED_CORPUS="${EXPECTED_CORPUS:-us-code}"

# Filled in by whoever installs, from the firm's own practice area:
#   GOLDEN_QUESTION      a question the us-code corpus can answer
#   GOLDEN_EXPECT_CORPUS the corpus its citations must come from
#   ABSTAIN_QUESTION     IN-DOMAIN but absent — see check 4
#   OCR_FIXTURE_PDF      a scanned PDF with no text layer
#   OCR_EXPECT_PHRASE    a phrase that appears in that scan's text
GOLDEN_QUESTION="${GOLDEN_QUESTION:-}"
GOLDEN_EXPECT_CORPUS="${GOLDEN_EXPECT_CORPUS:-$EXPECTED_CORPUS}"
ABSTAIN_QUESTION="${ABSTAIN_QUESTION:-}"
OCR_FIXTURE_PDF="${OCR_FIXTURE_PDF:-}"
OCR_EXPECT_PHRASE="${OCR_EXPECT_PHRASE:-}"

SVRN="${SVRN:-/opt/firm-rag/bin/svrn}"

# ── Verdict bookkeeping ──────────────────────────────────────────────
declare -a NAMES=() VERDICTS=() DETAILS=()
FAILED=0
UNSURE=0

_record() { NAMES+=("$1"); VERDICTS+=("$2"); DETAILS+=("$3"); }
ok()     { _record "$1" pass   "$2"; printf '  \033[32mpass\033[0m   %s\n' "$1"; }
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

if [ -z "$BASE_URL" ]; then
    echo "acceptance: BASE_URL is unset. Set it to the TLS front door," >&2
    echo "            e.g. BASE_URL=https://firm-rag.example.com" >&2
    echo "            (Pointing it at 127.0.0.1:8080 bypasses nginx and" >&2
    echo "             makes check 0 meaningless.)" >&2
    exit 2
fi
BASE_URL="${BASE_URL%/}"

case "$BASE_URL" in
    *127.0.0.1*|*localhost*)
        echo "acceptance: warning — BASE_URL points at loopback. Checks 0 and 0b" >&2
        echo "            assert on nginx's route allowlist and on the auth layer" >&2
        echo "            as a remote caller sees them. Re-run against the TLS" >&2
        echo "            hostname before signing anything off." >&2
        ;;
esac

# curl wrapper: prints "<http_code>\n<body>". --max-time is generous
# because a grounded turn on a 35B model is minutes, not seconds.
req() {
    local method="$1" path="$2" body="${3:-}"
    local -a args=(-sS -o - -w '\n%{http_code}' --max-time 900 -X "$method" "$BASE_URL$path")
    [ -n "$API_KEY" ] && args+=(-H "Authorization: Bearer $API_KEY")
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
# something that isn't 404", i.e. check 0 reports the dangerous routes as
# REACHABLE on a box where nothing is running. Caught by running this
# script against a dead port before trusting it; the verdict has to be
# could-not-judge, never fail-or-pass.
no_response() { [ -z "$1" ] || [ "$1" = "000" ]; }

echo
echo "acceptance: $BASE_URL  (daemon $DAEMON_URL)"
echo

# ─────────────────────────────────────────────────────────────────────
# 0 — SECURITY: the hardened binary is the one installed
#
# Proves the deployed `sovereign-server` was built --no-default-features.
# A default-features binary here means any tenant key is a shell
# (`test_command` reaches `sh -c` inside the AUTHENTICATED router) and
# any tenant can ingest an absolute server-side path — including the
# config holding every other tenant's key.
#
# 404 and not 405: the route must not exist. A 405 would mean the path is
# registered and only the method is wrong.
# ─────────────────────────────────────────────────────────────────────
for probe in "POST /v1/solve" "POST /v1/cycle/bdd" "POST /v1/documents/upload" "POST /v1/corpora/upload" "GET /mcp" "GET /mcp/stats"; do
    m="${probe%% *}"; p="${probe#* }"
    r="$(req "$m" "$p" '{}')"; c="$(code_of "$r")"
    if [ "$c" = "404" ]; then
        ok "0  $probe is gone" "404"
    elif no_response "$c"; then
        unsure "0  $probe" "no HTTP response from $BASE_URL — service down, DNS failed, or TLS rejected. NOT a pass: nothing was proven about this route."
    else
        bad "0  $probe is REACHABLE" "expected 404, got $c. Either the installed binary was not built --no-default-features, or nginx is not the front door."
    fi
done

# ─────────────────────────────────────────────────────────────────────
# 0b — SECURITY: authentication is actually ON
#
# This one is here because the failure is SILENT. `sovereign-server`
# enables auth only when `[auth] mode == "api_key"` AND `[auth] keys` is
# non-empty. `mode = "api_key"` with an empty map does not fail and does
# not warn — it serves every /v1/* route unauthenticated as tenant
# "default". The startup exposure guard does not catch it either,
# because `bind` is loopback.
#
# A no-token request MUST be refused. If it is not, every document on
# this box is readable by anyone who can reach the hostname.
# ─────────────────────────────────────────────────────────────────────
r="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 30 "$BASE_URL/v1/corpora" 2>/dev/null)"
if [ "$r" = "401" ]; then
    ok "0b unauthenticated request is refused" "401"
elif no_response "$r"; then
    unsure "0b unauthenticated request" "no HTTP response from $BASE_URL — cannot judge whether auth is on"
else
    bad "0b UNAUTHENTICATED REQUEST WAS SERVED" "GET /v1/corpora with no token returned $r, expected 401. \`[auth] keys\` is almost certainly empty in server-config.toml — which silently disables auth entirely."
fi

# ─────────────────────────────────────────────────────────────────────
# 0c — SECURITY: the egress tools are not on the agent runtime
#
# Three agent tools reached the open internet on ORDINARY CHAT TURNS and
# were governed by no config key, no env var, and no tool allowlist:
#
#   search's web fallback   html.duckduckgo.com → www.google.com →
#                           lite.duckduckgo.com, fired whenever the top
#                           LOCAL retrieval score was thin
#   web_fetch               any URL the model emits; scheme-only
#                           validation, no host allowlist
#   wikipedia_fetch         en.wikipedia.org
#
# `--no-default-features` did not remove them until the `net-tools`
# feature was added — it removed ShellTool, which sits three lines above
# them in the same function. This check is the proof that the fix is in
# the binary that got installed, and it is the check that would have
# caught the gap in the first place.
#
# `search` MUST still be present: it is corpus search, which is the
# product. Only its web backend went away.
# ─────────────────────────────────────────────────────────────────────
r="$(req GET /v1/tools)"; c="$(code_of "$r")"; b="$(body_of "$r")"
if no_response "$c"; then
    unsure "0c egress tools are not registered" "no HTTP response from $BASE_URL"
elif [ "$c" != "200" ]; then
    unsure "0c egress tools are not registered" "GET /v1/tools returned $c — cannot enumerate the registry"
else
    tool_names="$(printf '%s' "$b" | jq -r '[.. | objects | .name? // empty] | unique | .[]' 2>/dev/null)"
    if [ -z "$tool_names" ]; then
        unsure "0c egress tools are not registered" "could not parse tool names out of GET /v1/tools: $(printf '%s' "$b" | head -c 200)"
    else
        leaked=""
        for t in web_fetch wikipedia_fetch; do
            printf '%s\n' "$tool_names" | grep -qx "$t" && leaked="$leaked $t"
        done
        if [ -n "$leaked" ]; then
            bad "0c egress tools are not registered" "still registered:$leaked. This binary was not built with --no-default-features, or predates the net-tools feature. These reach the open internet on ordinary chat turns."
        elif ! printf '%s\n' "$tool_names" | grep -qx "search"; then
            bad "0c egress tools are not registered" "the egress tools are gone, but so is \`search\` — corpus search should survive --no-default-features. Retrieval may be crippled."
        else
            ok "0c egress tools are not registered" "web_fetch/wikipedia_fetch absent; corpus search present"
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
# `transitioning: true` means residency was indeterminate at read time
# (the slot lock was contended) — that is neither a pass nor a fail, it
# is "ask again", so it is reported as UNSURE rather than silently
# treated as either.
# ─────────────────────────────────────────────────────────────────────
st="$(curl -sS --max-time 30 "$DAEMON_URL/status" 2>/dev/null)"
if [ -z "$st" ] || ! printf '%s' "$st" | jq -e . >/dev/null 2>&1; then
    unsure "1  model slots resident" "daemon at $DAEMON_URL returned no parseable /status"
else
    for role in primary fast embed; do
        slot="$(printf '%s' "$st" | jq -c --arg r "$role" '.inference.resident[]? | select(.role == $r)' 2>/dev/null)"
        if [ -z "$slot" ]; then
            bad "1  slot '$role' resident" "no entry with role=\"$role\" in .inference.resident[] — the slot is not configured at all"
        elif [ "$(printf '%s' "$slot" | jq -r '.transitioning')" = "true" ]; then
            unsure "1  slot '$role' resident" "transitioning=true — residency indeterminate at read time; re-run"
        elif [ "$(printf '%s' "$slot" | jq -r '.resident')" = "true" ]; then
            ok "1  slot '$role' resident" "$(printf '%s' "$slot" | jq -r '.model_id')"
        else
            bad "1  slot '$role' resident" "resident=false (model_id=$(printf '%s' "$slot" | jq -r '.model_id')). With primary_idle_secs=86400 this should not idle-unload; check the daemon log for a load failure."
        fi
    done
fi

# ─────────────────────────────────────────────────────────────────────
# 2 — CORPUS: the prebuilt knowledge is installed and visible
#
# `GET /v1/corpora` reflects the `[retrieval] corpora` allow-list, so
# this also proves that list is not misspelled — a typo there is silent
# (no deny_unknown_fields) and would scope retrieval to nothing.
# ─────────────────────────────────────────────────────────────────────
r="$(req GET /v1/corpora)"; c="$(code_of "$r")"; b="$(body_of "$r")"
if no_response "$c"; then
    unsure "2  corpus '$EXPECTED_CORPUS' listed" "no HTTP response from $BASE_URL"
elif [ "$c" != "200" ]; then
    bad "2  corpus '$EXPECTED_CORPUS' listed" "GET /v1/corpora returned $c"
elif printf '%s' "$b" | jq -e --arg id "$EXPECTED_CORPUS" '[.. | .corpus_id? // .id? // empty] | index($id)' >/dev/null 2>&1; then
    ok "2  corpus '$EXPECTED_CORPUS' listed" ""
else
    bad "2  corpus '$EXPECTED_CORPUS' listed" "not in the response. Either the snapshot restore did not land, or [retrieval] corpora in server-config.toml does not name it. Got: $(printf '%s' "$b" | head -c 300)"
fi

# ── Helper: run one turn, echo the assistant MessageResponse ─────────
ask() {
    local question="$1"
    local conv cid
    conv="$(req POST /v1/conversations '{}')"
    [ "$(code_of "$conv")" = "200" ] || [ "$(code_of "$conv")" = "201" ] || { printf ''; return 1; }
    cid="$(body_of "$conv" | jq -r '.id // .conversation_id // empty')"
    [ -n "$cid" ] || { printf ''; return 1; }
    local payload
    payload="$(jq -nc --arg c "$question" '{content: $c}')"
    local resp
    resp="$(req POST "/v1/conversations/$cid/messages" "$payload")"
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
        unsure "3  grounded answer has citations" "the turn did not complete (see the server journal)"
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
        unsure "4  abstains on the unsourceable" "the turn did not complete (see the server journal)"
    else
        v="$(printf '%s' "$msg" | jq -r '.epistemic_state.verdict // "ABSENT"')"
        case "$v" in
            cannot_know_from_here)
                ok "4  abstains on the unsourceable" "verdict=cannot_know_from_here" ;;
            ABSENT)
                bad "4  abstains on the unsourceable" "no epistemic_state on the response at all. The turn stamped no ledger — check that the grounding gate ran (a zero-chunk turn takes the retrieval-miss path and produces no gate metadata)." ;;
            general_knowledge|mixed)
                bad "4  abstains on the unsourceable" "verdict=$v — the box answered from parametric knowledge instead of abstaining. If the probe is out-of-domain it tripped gk_rescue; make it in-domain-but-absent." ;;
            *)
                bad "4  abstains on the unsourceable" "verdict=$v, expected cannot_know_from_here" ;;
        esac
    fi
fi

# ─────────────────────────────────────────────────────────────────────
# 5 — OCR: a scanned PDF produces text
#
# For a litigation practice, scanned PDFs are not an edge case — they are
# the corpus. Two assertions, because either alone is weak:
#
#   (a) NEGATIVE: the file is not in the sweep's failed_files under
#       `scanned_no_text`. That reason is what you get when the daemon
#       was built without --features ocr, or built with it and could not
#       resolve its models.
#   (b) POSITIVE: a phrase known to be in the scan comes back from
#       search. (a) alone only proves nothing complained.
# ─────────────────────────────────────────────────────────────────────
if [ -z "$OCR_FIXTURE_PDF" ] || [ -z "$OCR_EXPECT_PHRASE" ]; then
    unsure "5  OCR reads a scanned PDF" "OCR_FIXTURE_PDF / OCR_EXPECT_PHRASE unset in $PROBES_FILE. Use one of the firm's own scans — our test images say nothing about their scanner, their DPI, or their paper."
elif [ ! -f "$OCR_FIXTURE_PDF" ]; then
    unsure "5  OCR reads a scanned PDF" "fixture not found at $OCR_FIXTURE_PDF"
elif [ ! -x "$SVRN" ]; then
    unsure "5  OCR reads a scanned PDF" "$SVRN not executable; set SVRN=<path>"
else
    ocr_dir="$(mktemp -d)"
    cp "$OCR_FIXTURE_PDF" "$ocr_dir/" 2>/dev/null
    # `--sync-initial` makes the register call BLOCK on the first sweep,
    # so there is nothing to poll for. The corpus id is derived by the
    # daemon (not from --name, which sets only the display name), so read
    # it back off stdout rather than guessing at the slug rule.
    reg="$("$SVRN" corpus watch "$ocr_dir" --name "acceptance-ocr-$$" --ocr --sync-initial 2>&1)"
    ocr_cid="$(printf '%s\n' "$reg" | sed -n 's/^ *corpus_id *= *//p' | head -n1 | tr -d '[:space:]')"
    if [ -z "$ocr_cid" ]; then
        unsure "5  OCR reads a scanned PDF" "\`svrn corpus watch --ocr --sync-initial\` did not report a corpus_id: $(printf '%s' "$reg" | head -c 300)"
    else
        state="$("$SVRN" corpus watch-status "$ocr_cid" --failures 2>&1)"
        if printf '%s' "$state" | grep -qi 'scanned_no_text'; then
            bad "5  OCR reads a scanned PDF" "the scan landed in failed_files as scanned_no_text. Either the daemon was not built --features ocr, or it could not resolve the PaddleOCR models / libpdfium. The journal names which: grep for 'ocr:unavailable', which lists every path it probed."
        else
            # Search the daemon-side corpus directly, NOT POST /v1/search.
            # The server's [retrieval] corpora allow-list does not contain
            # this temporary corpus, so the server route would return
            # nothing and the check would fail for the wrong reason.
            hit="$("$SVRN" corpus search "$ocr_cid" "$OCR_EXPECT_PHRASE" --limit 5 2>&1)"
            if printf '%s' "$hit" | grep -qiF "$OCR_EXPECT_PHRASE"; then
                ok "5  OCR reads a scanned PDF" "extracted text is searchable"
            else
                bad "5  OCR reads a scanned PDF" "the file was not reported as failed, but '$OCR_EXPECT_PHRASE' does not come back from search. OCR likely produced empty or garbled text — check the daemon log for the '<!-- raw OCR (cleanup unavailable) -->' marker, which means the cleanup model id was wrong (it must be a GGUF file stem, never a slot alias like \"fast\")."
            fi
        fi
        "$SVRN" corpus watch-remove "$ocr_cid" >/dev/null 2>&1 || true
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
echo "routes that could reach a shell are not on it."
exit 0
