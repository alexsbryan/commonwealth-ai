#!/usr/bin/env bash
# canon-export-notes.py — what leaves the notes store for a committed canon
# source, and what does not.
#
# WHY THIS EXISTS. The exporter's output is COMMITTED under .canon/sources/ in a
# public repository, so what it refuses and what it redacts matter more than
# what it writes. A retired, tombstoned or private note in git is a leak nobody
# can recall; an email or a tailnet address that survives redaction is the same;
# a project memory exported as guidance puts state into a rulebook; a short-id
# collision would point two canon rules at one note's history. Each case is a
# row in a throwaway store and a throwaway memory directory — nothing reads
# ~/.svrnmesh or ~/.claude. The negative control re-marks the retired row live
# and requires it to appear, so "nothing retired was exported" cannot pass by
# exporting nothing.
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

MIG=memory-migration-2026-07-18
ROWS='[
 ("aaaaaaaa-0001","invariant","**Applies to:** all machines\n\n**EVERY PROMPT GOES THROUGH bounded_evidence** - watched red.\nThe glob `**/*.rs` stays as written.\nPeer at 100.115.12.21 or 192.168.1.2, loopback 127.0.0.1, mail someone@example.com, file /Users/someone/dev/x.rs.","mcp",1786000000,None,0,0,"h1"),
 ("bbbbbbbb-0002","invariant","Retired rule.","mcp",1786000000,1786000500,0,0,"h2"),
 ("cccccccc-0003","invariant","Tombstoned rule.","mcp",1786000000,None,1,0,"h3"),
 ("dddddddd-0004","attempt","Private attempt.","mcp",1786000000,None,0,1,"h4"),
 ("eeeeeeee-0005","decision","**Applies to:** all\n\nKeep canon simple and prove every capability on our side before promoting it. **Why:** leanness.","'"$MIG"'",1786000000,None,0,0,"h5"),
 ("ffffffff-0006","decision","A project log describing where the enrichment work stood that week, with numbers.","'"$MIG"'",1786000000,None,0,0,"h6"),
 ("99999999-0008","decision","An orphan migrated memory whose original file is gone from the directory entirely.","'"$MIG"'",1786000000,None,0,0,"h8"),
 ("12345678-0007","decision","**Why:** an ordinary session decision that was never a memory at all.","mcp",1786000000,None,0,0,"h7")
]'

MEM="$T/memory"; mkdir -p "$MEM"
printf -- '---\nname: keep-simple\ndescription: Keep canon simple\nmetadata:\n  type: feedback\n---\n\nKeep canon simple and prove every capability on our side before promoting it. **Why:** leanness.\n' > "$MEM/feedback_keep_simple.md"
printf -- '---\nname: log\ndescription: Enrichment log\nmetadata:\n  type: project\n---\n\nA project log describing where the enrichment work stood that week, with numbers.\n' > "$MEM/project_log.md"
printf -- '---\nname: redact\ndescription: "Never publish internal notes unredacted"\nmetadata:\n  type: feedback\n---\n\nRedact emails, private addresses and home paths before anything leaves the machine for a public repository.\n' > "$MEM/feedback_redact.md"
printf -- '---\nname: state\ndescription: Some state\nmetadata:\n  type: project\n---\n\nSome project state that exists only in the harness memory directory and is not guidance.\n' > "$MEM/project_state.md"
printf -- '# Memory Index\n- [x](feedback_keep_simple.md)\n' > "$MEM/MEMORY.md"

echo "canon-export-notes:"
mkstore "$T/store.db" "$ROWS"
OUT="$T/out"
python3 "$SCRIPT" --db "$T/store.db" --memory-dir "$MEM" --out "$OUT" 2>"$T/err1"; code=$?
[[ $code -eq 0 ]] && ok "exports from a readable store (exit 0)" || fail "first export exited $code: $(cat "$T/err1")"

[[ -f "$OUT/invariant/aaaaaaaa.md" && -f "$OUT/memory/eeeeeeee.md" && -f "$OUT/harness/feedback_redact.md" ]] \
  && ok "a live invariant, a feedback-typed migrated memory and an unmigrated feedback file are written" \
  || fail "expected invariant/aaaaaaaa.md, memory/eeeeeeee.md, harness/feedback_redact.md; got: $(cd "$OUT" 2>/dev/null && find . -name '*.md' | sort | tr '\n' ' ')"

for short in bbbbbbbb cccccccc dddddddd ffffffff 99999999 12345678 feedback_keep_simple project_log project_state; do
  if find "$OUT" -name "$short.md" | grep -q .; then
    fail "$short must not be exported"
  else
    ok "$short is not exported"
  fi
done

F="$OUT/invariant/aaaaaaaa.md"
first="$(head -1 "$F")"
[[ "$first" == "# EVERY PROMPT GOES THROUGH bounded_evidence - watched red." ]] \
  && ok "heading is the note's first line without markup" || fail "heading was: $first"
[[ "$(head -1 "$OUT/harness/feedback_redact.md")" == "# Never publish internal notes unredacted" ]] \
  && ok "a harness file's heading is its description" || fail "harness heading: $(head -1 "$OUT/harness/feedback_redact.md")"
grep -q "Applies to" "$F" && fail "migration boilerplate survived" || ok "migration boilerplate is dropped"
grep -qF '`**/*.rs`' "$F" && ok "bold is not unwrapped inside a code span" || fail "the glob in a code span was altered"
grep -qF '**EVERY' "$F" && fail "bold outside a code span was left in the body" || ok "bold outside code spans is unwrapped"

for leak in "someone@example.com" "100.115.12.21" "192.168.1.2" "/Users/someone"; do
  grep -qF "$leak" "$F" && fail "redaction missed $leak" || ok "redacted: $leak"
done
grep -qF "127.0.0.1" "$F" && grep -qF "~/dev/x.rs" "$F" && grep -qF "<tailnet-ip>" "$F" && grep -qF "<lan-ip>" "$F" && grep -qF "<email>" "$F" \
  && ok "loopback is kept and each redaction leaves its marker" || fail "redacted body: $(tail -1 "$F")"
grep -q "redacted email 1, tailnet-ip 1, lan-ip 1, home-path 1" "$T/err1" \
  && ok "redactions are counted" || fail "summary was: $(cat "$T/err1")"

[[ "$(wc -l < "$OUT/MANIFEST.jsonl" | tr -d ' ')" == "3" ]] \
  && ok "manifest has one row per exported note" || fail "manifest rows: $(wc -l < "$OUT/MANIFEST.jsonl")"
grep -q "private withheld 1" "$T/err1" \
  && ok "the private note is counted as withheld, not silently absent" || fail "summary was: $(cat "$T/err1")"
grep -q "1 feedback/invariant exported, 1 project/reference not exported, 1 untyped not exported" "$T/err1" \
  && ok "every migrated memory's typing decision is counted" || fail "summary was: $(cat "$T/err1")"
grep -q "1 feedback/invariant exported, 1 already migrated, 2 project/reference not exported, 0 too short to match" "$T/err1" \
  && ok "every memory file's decision is counted" || fail "summary was: $(cat "$T/err1")"

python3 "$SCRIPT" --db "$T/store.db" --memory-dir "$MEM" --out "$OUT" 2>"$T/err2"
grep -q "written 0, unchanged 3" "$T/err2" \
  && ok "a re-run writes nothing" || fail "re-run summary: $(cat "$T/err2")"

python3 "$SCRIPT" --db "$T/store.db" --out "$OUT" --class attempt 2>/dev/null
[[ "$(wc -l < "$OUT/MANIFEST.jsonl" | tr -d ' ')" == "3" ]] \
  && ok "a single-class run keeps the other classes' manifest rows" || fail "manifest after --class attempt: $(cat "$OUT/MANIFEST.jsonl")"

python3 "$SCRIPT" --db "$T/store.db" --out "$T/nodir" --class memory 2>"$T/err4"; code=$?
[[ $code -eq 2 ]] && grep -q "need --memory-dir" "$T/err4" \
  && ok "typing a memory without its memory directory is refused, not guessed" || fail "memory without --memory-dir: exit $code, $(cat "$T/err4")"

# Negative control: the same retired row, marked live, must now appear.
python3 - "$T/store.db" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1]); con.execute("UPDATE notes SET retired_at = NULL WHERE id = 'bbbbbbbb-0002'"); con.commit()
PY
python3 "$SCRIPT" --db "$T/store.db" --out "$OUT" --class invariant 2>/dev/null
[[ -f "$OUT/invariant/bbbbbbbb.md" ]] \
  && ok "negative control: the same row, live, is exported" || fail "negative control: a live row was not exported — the exclusion checks above prove nothing"

mkstore "$T/collide.db" '[("abcdefab-0001","invariant","One.","mcp",1,None,0,0,"x"),("abcdefab-0002","invariant","Two.","mcp",1,None,0,0,"y")]'
python3 "$SCRIPT" --db "$T/collide.db" --out "$T/collide" --class invariant 2>"$T/err3"; code=$?
[[ $code -eq 1 ]] && grep -q "short-id collision abcdefab" "$T/err3" && [[ ! -d "$T/collide" ]] \
  && ok "a short-id collision fails before writing anything" || fail "collision: exit $code, $(cat "$T/err3")"

python3 "$SCRIPT" --db "$T/missing.db" --out "$T/none" --class invariant 2>/dev/null; code=$?
[[ $code -eq 1 ]] && ok "a missing store is an error, not an empty export" || fail "missing store exited $code"

exit $rc
