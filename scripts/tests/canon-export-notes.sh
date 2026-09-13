#!/usr/bin/env bash
# canon-export-notes.py — what leaves the notes store for a committed canon
# source, and what does not.
#
# WHY THIS EXISTS. The exporter's output is COMMITTED under .canon/sources/, so
# what it refuses matters more than what it writes. A retired, tombstoned or
# private note exported into git is a leak nobody can recall; a short-id
# collision would point two canon rules at one note's history; a glob like
# `**/*.rs` unwrapped as bold would corrupt a rule's evidence. Each case is a
# row in a throwaway store the script is pointed at — nothing reads
# ~/.svrnmesh. The negative control re-marks the retired row live and requires
# it to appear, so "nothing retired was exported" cannot pass by exporting
# nothing.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
SCRIPT="$ROOT/scripts/canon-export-notes.py"
[[ -f "$SCRIPT" ]] || { echo "cannot find $SCRIPT"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
rc=0
ok()   { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; rc=1; }

mkstore() {  # mkstore <db> <python rows literal>
python3 - "$1" "$2" <<'PY'
import sqlite3, sys, ast
con = sqlite3.connect(sys.argv[1])
con.execute("""CREATE TABLE notes (id TEXT PRIMARY KEY, kind TEXT, content TEXT,
  session_id TEXT, created_at INTEGER, retired_at INTEGER, tombstone INTEGER DEFAULT 0,
  private INTEGER DEFAULT 0, content_hash TEXT)""")
for r in ast.literal_eval(sys.argv[2]):
    con.execute("INSERT INTO notes VALUES (?,?,?,?,?,?,?,?,?)", r)
con.commit()
PY
}

ROWS='[
 ("aaaaaaaa-0001","invariant","**Applies to:** all machines\n\n**EVERY PROMPT GOES THROUGH bounded_evidence** - watched red.\nThe glob `**/*.rs` stays as written.","mcp",1786000000,None,0,0,"h1"),
 ("bbbbbbbb-0002","invariant","Retired rule.","mcp",1786000000,1786000500,0,0,"h2"),
 ("cccccccc-0003","invariant","Tombstoned rule.","mcp",1786000000,None,1,0,"h3"),
 ("dddddddd-0004","attempt","Private attempt.","mcp",1786000000,None,0,1,"h4"),
 ("eeeeeeee-0005","decision","**Keep canon simple**\n\n**Why:** leanness.","memory-migration-2026-07-18",1786000000,None,0,0,"h5"),
 ("ffffffff-0006","decision","A project log with no reason line.","memory-migration-2026-07-18",1786000000,None,0,0,"h6"),
 ("12345678-0007","decision","**Why:** an ordinary session decision.","mcp",1786000000,None,0,0,"h7")
]'

echo "canon-export-notes:"
mkstore "$T/store.db" "$ROWS"
OUT="$T/out"
python3 "$SCRIPT" --db "$T/store.db" --out "$OUT" 2>"$T/err1"; code=$?
[[ $code -eq 0 ]] && ok "exports from a readable store (exit 0)" || fail "first export exited $code: $(cat "$T/err1")"

[[ -f "$OUT/invariant/aaaaaaaa.md" && -f "$OUT/memory/eeeeeeee.md" ]] \
  && ok "a live invariant and a Why-bearing migrated memory are written" \
  || fail "expected invariant/aaaaaaaa.md and memory/eeeeeeee.md; got: $(cd "$OUT" 2>/dev/null && find . -name '*.md' | sort | tr '\n' ' ')"

for short in bbbbbbbb cccccccc dddddddd ffffffff 12345678; do
  if find "$OUT" -name "$short.md" | grep -q .; then
    fail "$short must not be exported (retired / tombstoned / private / no Why / not a migration)"
  else
    ok "$short is not exported"
  fi
done

first="$(head -1 "$OUT/invariant/aaaaaaaa.md")"
[[ "$first" == "# EVERY PROMPT GOES THROUGH bounded_evidence - watched red." ]] \
  && ok "heading is the note's first line without markup" || fail "heading was: $first"
grep -q "Applies to" "$OUT/invariant/aaaaaaaa.md" \
  && fail "migration boilerplate survived" || ok "migration boilerplate is dropped"
grep -qF '`**/*.rs`' "$OUT/invariant/aaaaaaaa.md" \
  && ok "bold is not unwrapped inside a code span" || fail "the glob in a code span was altered"
grep -qF '**EVERY' "$OUT/invariant/aaaaaaaa.md" \
  && fail "bold outside a code span was left in the body" || ok "bold outside code spans is unwrapped"

[[ "$(wc -l < "$OUT/MANIFEST.jsonl" | tr -d ' ')" == "2" ]] \
  && ok "manifest has one row per exported note" || fail "manifest rows: $(wc -l < "$OUT/MANIFEST.jsonl")"
grep -q "private withheld 1" "$T/err1" \
  && ok "the private note is counted as withheld, not silently absent" || fail "summary was: $(cat "$T/err1")"
grep -q "migrated memories without a Why line 1" "$T/err1" \
  && ok "memories the Why heuristic drops are counted" || fail "summary was: $(cat "$T/err1")"

python3 "$SCRIPT" --db "$T/store.db" --out "$OUT" 2>"$T/err2"
grep -q "written 0, unchanged 2" "$T/err2" \
  && ok "a re-run writes nothing" || fail "re-run summary: $(cat "$T/err2")"

python3 "$SCRIPT" --db "$T/store.db" --out "$OUT" --class attempt 2>/dev/null
[[ "$(wc -l < "$OUT/MANIFEST.jsonl" | tr -d ' ')" == "2" ]] \
  && ok "a single-class run keeps the other classes' manifest rows" || fail "manifest after --class attempt: $(cat "$OUT/MANIFEST.jsonl")"

# Negative control: the same retired row, marked live, must now appear.
python3 - "$T/store.db" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1]); con.execute("UPDATE notes SET retired_at = NULL WHERE id = 'bbbbbbbb-0002'"); con.commit()
PY
python3 "$SCRIPT" --db "$T/store.db" --out "$OUT" 2>/dev/null
[[ -f "$OUT/invariant/bbbbbbbb.md" ]] \
  && ok "negative control: the same row, live, is exported" || fail "negative control: a live row was not exported — the exclusion checks above prove nothing"

mkstore "$T/collide.db" '[("abcdefab-0001","invariant","One.","mcp",1,None,0,0,"x"),("abcdefab-0002","invariant","Two.","mcp",1,None,0,0,"y")]'
python3 "$SCRIPT" --db "$T/collide.db" --out "$T/collide" 2>"$T/err3"; code=$?
[[ $code -eq 1 ]] && grep -q "short-id collision abcdefab" "$T/err3" && [[ ! -d "$T/collide" ]] \
  && ok "a short-id collision fails before writing anything" || fail "collision: exit $code, $(cat "$T/err3")"

python3 "$SCRIPT" --db "$T/missing.db" --out "$T/none" 2>/dev/null; code=$?
[[ $code -eq 1 ]] && ok "a missing store is an error, not an empty export" || fail "missing store exited $code"

exit $rc
