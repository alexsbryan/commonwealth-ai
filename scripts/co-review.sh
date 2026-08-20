#!/usr/bin/env bash
# co-review.sh — the comaintainer's landing-review helper.
#
# Invoked by the director when a worker reports done (its verdict is
# then drafted to the operator like any directive); also runnable
# standalone for shadow sweeps over new commits. Spec:
# docs/COMAINTAINER.md §10 artifact 3; charter: gym/comaintainer/CHARTER.md.
#
#   scripts/co-review.sh [ref]                  # default HEAD
#   scripts/co-review.sh [ref] --engine claude  # frontier engine (budgeted)
#   scripts/co-review.sh [ref] --override "reason"  # log an operator override
#   scripts/co-review.sh [ref] --field          # + landing field-diff (opt-in;
#                                               # ledger: landing-field-diff)
#
# Exit codes: 0 always (the seat is advisory at M0 — no gate, no hook),
# 2 usage. Verdicts append to ~/.sovereign/comaintainer/verdicts.jsonl.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
GYM="$REPO/gym/comaintainer"
OUT_DIR="$HOME/.sovereign/comaintainer"
LOG="$OUT_DIR/verdicts.jsonl"
DAEMON="${SOVEREIGN_DAEMON_URL:-http://localhost:9741}"

REF="HEAD"
ENGINE="daemon"
OVERRIDE=""
FIELD=""
while [ $# -gt 0 ]; do
  case "$1" in
    --engine) ENGINE="${2:?}"; shift 2 ;;
    --override) OVERRIDE="${2:?}"; shift 2 ;;
    --field) FIELD=1; shift ;;
    -h|--help) sed -n '2,16p' "$0"; exit 2 ;;
    -*) echo "co-review: unknown flag $1" >&2; exit 2 ;;
    *) REF="$1"; shift ;;
  esac
done

mkdir -p "$OUT_DIR"
COMMIT="$(git -C "$REPO" rev-parse "$REF" 2>/dev/null)" || {
  echo "co-review: unresolvable ref: $REF" >&2; exit 2; }

if [ -n "$OVERRIDE" ]; then
  # Logging the override is what mints the training episode.
  python3 - "$COMMIT" "$OVERRIDE" "$LOG" <<'PY'
import json, sys, datetime
commit, reason, log = sys.argv[1:4]
rec = {"ts": datetime.datetime.now(datetime.timezone.utc).isoformat(),
       "ref": commit, "kind": "override", "reason": reason}
open(log, "a").write(json.dumps(rec) + "\n")
print(f"override logged for {commit[:7]}: {reason}")
PY
  exit 0
fi

# ---- landing field-diff (--field, Scene 2) ---------------------------
# One degraded scratch render (--no-dup: the O(n^2) tier is the only
# slow stage), diffed against the standing sidecar by co-field.py.
# Doubly baseline-safe: degraded AND redirected via --out.
FIELD_DIR=""
FIELD_EV_TXT=""
FIELD_EV_JSON=""
if [ -n "$FIELD" ]; then
  FIELD_DIR="$(mktemp -d -t co-field.XXXXXX)"
  CORPUS="$(python3 "$REPO/scripts/co-field.py" corpus --repo "$REPO" 2>/dev/null || true)"
  echo "co-review: --field — scratch render (--no-dup; dup tier surfaces at next glance)" >&2
  if "$REPO/target/debug/sovereign-cli-dev" code fieldglass ${CORPUS:+$CORPUS} \
      --no-dup --out "$FIELD_DIR/landing.html" >"$FIELD_DIR/render.log" 2>&1; then
    python3 "$REPO/scripts/co-field.py" diff "$FIELD_DIR/landing.json" "$COMMIT" \
      --repo "$REPO" --json-out "$FIELD_DIR/evidence.json" \
      >"$FIELD_DIR/evidence.txt" 2>&1 || true
    FIELD_EV_TXT="$FIELD_DIR/evidence.txt"
    FIELD_EV_JSON="$FIELD_DIR/evidence.json"
  else
    echo "co-review: scratch render FAILED (log kept: $FIELD_DIR/render.log) — field diff is could-not-judge(missing: scratch render), reported not defaulted" >&2
  fi
fi

# ---- assemble the landing bundle -------------------------------------
# Every absent artifact is NAMED in the bundle, never silently omitted
# (ARCH §18.3).
BUNDLE="$(mktemp -t co-review-bundle.XXXXXX)"
{
  echo "=== COMMIT $COMMIT ==="
  git -C "$REPO" log -1 --format='%s%n%n%b' "$COMMIT"
  echo "=== DIFFSTAT ==="
  git -C "$REPO" show --stat --format= "$COMMIT" | tail -40
  echo "=== DIFF (truncated) ==="
  git -C "$REPO" show --format= --no-color "$COMMIT" | head -c 24000
  echo
  echo "=== GATE ARTIFACTS ==="
  for gate in sovereign-lint sovereign-test; do
    f="$REPO/target/$gate/latest"
    if [ -d "$f" ]; then
      echo "--- $gate/latest summary:"
      head -c 1200 "$f"/*.txt 2>/dev/null || echo "(no summary file in $f)"
    else
      echo "--- $gate: ABSENT — no run artifact on this host (named, not omitted)"
    fi
  done
  echo "=== FIELD EVIDENCE (fieldglass sidecar) ==="
  # Standing-field evidence for the changed files (docs/FIELD_VERDICTS.md
  # Scene 1). co-field.py names every absence itself, so this section can
  # never be silently empty.
  git -C "$REPO" diff-tree -r --name-only --no-commit-id "$COMMIT" \
    | python3 "$REPO/scripts/co-field.py" evidence "$COMMIT" --repo "$REPO"
  if [ -n "$FIELD_EV_TXT" ] && [ -r "$FIELD_EV_TXT" ]; then
    echo "--- landing field-diff (--field):"
    cat "$FIELD_EV_TXT"
  elif [ -n "$FIELD" ]; then
    echo "--- landing field-diff: could-not-judge — scratch render failed (named, not omitted)"
  fi
  echo "=== MATCHED NOTES (path stems) ==="
  STEMS="$(git -C "$REPO" diff-tree -r --name-only --no-commit-id "$COMMIT" \
    | head -8 | xargs -n1 basename 2>/dev/null | sed 's/\.[^.]*$//' | sort -u | head -6)"
  if [ -r "$HOME/.sovereign/notes.db" ] && [ -n "$STEMS" ]; then
    for s in $STEMS; do
      sqlite3 -readonly "$HOME/.sovereign/notes.db" \
        "SELECT '['||kind||'] '||substr(replace(content,char(10),' '),1,200)
         FROM notes WHERE tombstone=0 AND retired_at IS NULL
         AND (files LIKE '%$s%' OR content LIKE '%$s%') LIMIT 2" 2>/dev/null
    done | head -20
  else
    echo "notes.db ABSENT or no changed paths — notes context missing (named)"
  fi
  echo "=== LEDGER MENTIONS OF TOUCHED FLAGS ==="
  FLAGS="$(git -C "$REPO" show --format= "$COMMIT" | grep -oE 'SOVEREIGN_[A-Z_]+' | sort -u | head -6)"
  if [ -n "$FLAGS" ]; then
    for f in $FLAGS; do grep -n "$f" "$REPO/sovereign/DEFAULTS_LEDGER.md" | head -2; done
  else
    echo "(no SOVEREIGN_* flags in this diff)"
  fi
} > "$BUNDLE"

# ---- one model call, schema-validated verdict ------------------------
python3 - "$BUNDLE" "$GYM" "$ENGINE" "$COMMIT" "$LOG" "$DAEMON" "$FIELD_EV_JSON" <<'PY'
import json, sys, hashlib, datetime, subprocess, tempfile, urllib.request
from pathlib import Path
bundle_p, gym, engine, commit, log, daemon = sys.argv[1:7]
field_ev_p = sys.argv[7] if len(sys.argv) > 7 else ""
sys.path.insert(0, gym)
import markers as M
from score import basis_gate, extract_verdict, call_daemon, call_claude  # noqa

charter = (Path(gym) / "CHARTER.md").read_text()
contract = (Path(gym) / "contract.txt").read_text()
bundle = Path(bundle_p).read_text()
prompt = (charter.strip() + "\n\n=== OUTPUT CONTRACT ===\n" + contract.strip()
          + "\n\n=== LANDING UNDER REVIEW (commit " + commit[:7] + ") ===\n"
          + bundle + "\n\nReturn the verdict JSON now.")
try:
    if engine == "daemon":
        # Schema-FORCED, unlike the gym's measurement runs: the reply is
        # sampled under a grammar built from contract.txt, so a missing
        # argument field or a non-verdict string cannot be generated.
        # could-not-judge survives for what it should mean — the engine
        # was unreachable, or the judge honestly refused (verdict
        # "could-not-judge" is one of the six branches).
        completion, model = call_daemon(prompt, 600.0, 700,
                                        schema=M.verdict_schema())
    else:
        # `claude -p` has no grammar seam; the contract text is the only
        # constraint on that engine, and extract_verdict still judges it.
        completion, model = call_claude(prompt, 600.0, None)
except Exception as e:
    print(f"co-review: engine error ({type(e).__name__}: {e}) — verdict is "
          f"could-not-judge(engine unavailable), reported not defaulted", file=sys.stderr)
    completion, model = "", engine + "-unavailable"

parsed, malformed = extract_verdict(completion) if completion else (None, "engine_error")
rec = {"ts": datetime.datetime.now(datetime.timezone.utc).isoformat(),
       "ref": commit, "engine": engine, "model": model,
       "charter_sha256": hashlib.sha256((Path(gym)/"CHARTER.md").read_bytes()).hexdigest(),
       "malformed": malformed}
# Stamp which field snapshot any field: citation resolves against
# (docs/FIELD_VERDICTS.md Scene 1 — the audit key for evidence age).
_sc_path, _sc_how = M.sidecar_path(Path(gym).resolve().parent.parent)
try:
    _sc = json.loads(_sc_path.read_text()) if _sc_path else None
except (OSError, json.JSONDecodeError):
    _sc = None
rec.update({"sidecar_head": _sc.get("head") if _sc else None,
            "sidecar_unix": _sc.get("generated_unix") if _sc else None,
            "sidecar_how": _sc_how})
if field_ev_p:
    try:
        rec["field_evidence"] = json.loads(Path(field_ev_p).read_text())
    except (OSError, json.JSONDecodeError):
        rec["field_evidence"] = {"status": "could-not-judge",
                                 "missing": "field diff output"}
if parsed and not malformed:
    v = parsed["verdict"]
    # G1 — resolve the citations HERE, in the live path. The gate itself
    # is score.basis_gate (one implementation; co-role.py's R4 takes the
    # same one). Until 2026-08-19 this line copied `basis` straight into
    # the record and co-closeout.py rendered it as if checked.
    rec.update({"verdict": v, M.ARG_OF[v]: parsed.get(M.ARG_OF[v]),
                "rationale": parsed.get("rationale", "")})
    rec.update(basis_gate(parsed))
    arg = parsed.get(M.ARG_OF[v])
    if rec.get("basis_unresolved"):
        print(f"VERDICT could-not-judge — engine proposed {v!r} on "
              f"{len(rec['basis_unresolved'])} unresolvable anchor(s): "
              f"{rec['basis_unresolved']}")
        print(f"  rationale: {parsed.get('rationale','')[:300]}")
    else:
        note = "" if rec.get("basis_checked") else "  (NOT verified)"
        print(f"VERDICT {v} — {M.ARG_OF[v]}: {json.dumps(arg, ensure_ascii=False)[:200]}")
        print(f"  basis: {rec['basis']}{note}")
        print(f"  rationale: {parsed.get('rationale','')[:300]}")
else:
    rec["verdict"] = "could-not-judge"
    rec["missing"] = f"a well-formed engine reply ({malformed})"
    rec["raw_head"] = (completion or "")[:400]
    print(f"VERDICT could-not-judge — {malformed} (raw head logged)")
Path(log).parent.mkdir(parents=True, exist_ok=True)
with open(log, "a") as fh:
    fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
print(f"appended -> {log}")
PY

rm -f "$BUNDLE"
[ -n "$FIELD_DIR" ] && rm -rf "$FIELD_DIR"

# ---- closure candidates: an ACCELERATOR, never the mechanism ----------
# This sweep already model-reads every landed commit, so asking "does it
# also close an open backlog item?" is nearly free. What it buys is that
# the level-triggered liveness pass finds less to do.
#
# IT IS NOT THE LOOP. scripts/co_liveness.py answers "does this item
# reproduce at HEAD?" from the tree alone, with no mark and no cursor, so
# the heap is correct while this sweep is behind (it has hit its 20-commit
# cap six nights running), uninstalled, or permanently off. If this step
# ever becomes load-bearing it has to be demoted again — the order's seam
# says so explicitly.
#
# Bounded: a lexical prefilter picks at most 3 items per commit, so the
# cost per commit does not grow with the backlog. Failure here never
# fails the review — proposals are a bonus, and a bonus that can redden a
# gate is a gate.
# NOTHING AUTO-RETIRES: candidates append to the same verdicts log for
# the seat to dispose of.
#
# INTERPRETER: this step reaches co-backlog.py's store reader, which
# parses the ruler with `tomllib` (3.11+). Under launchd — which is how
# co-sweep.sh runs every night — PATH is minimal and `python3` resolves
# to the system 3.9, where that import fails. So pick an interpreter that
# can actually do the work, and if there is none, SAY the step was
# skipped and why (ARCH §18.3) rather than emitting a stack trace that
# reads like a broken sweep.
if [ "${CO_CLOSURE_CANDIDATES:-1}" != "0" ]; then
  CLOSURE_PY=""
  for cand in "${SOVEREIGN_PYTHON:-}" python3.13 python3.12 python3.11 python3 \
              /Library/Frameworks/Python.framework/Versions/3.13/bin/python3 \
              /opt/homebrew/bin/python3; do
    [ -n "$cand" ] || continue
    command -v "$cand" >/dev/null 2>&1 || continue
    if "$cand" -c 'import tomllib' >/dev/null 2>&1; then CLOSURE_PY="$cand"; break; fi
  done
  if [ -n "$CLOSURE_PY" ]; then
    "$CLOSURE_PY" "$REPO/scripts/co_liveness.py" candidates "$COMMIT" 2>&1 \
      | sed 's/^/co-review: /' || true
  else
    echo "co-review: closure candidates SKIPPED — no python3 with tomllib on" \
         "PATH (set SOVEREIGN_PYTHON). Named, not silent; the backlog's" \
         "liveness loop does not depend on this step." >&2
  fi
fi
exit 0
