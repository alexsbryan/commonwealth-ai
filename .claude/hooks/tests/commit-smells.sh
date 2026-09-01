#!/bin/bash
# commit-smells.py end to end: the REAL hook, the REAL engine
# (scripts/co-arch.py) and the REAL rule set (quality/arch-probes.toml)
# against a throwaway git repo. Each case is a failure shape the hook must
# catch or must not produce (ARCH §18.1: a gate nobody watched fail is not
# a gate):
#
#   - a non-commit Bash call is silent and spawns nothing
#   - a staged 4-arm string match + unwrap_or blocks (exit 2) with sites
#   - the same sites never block twice in one session; a new session sees them
#   - `git add X && git commit` is audited as ONE call: X's sites are shown
#     and the REAL index is untouched (the replay went to a temp index)
#   - `git commit -a` picks up the unstaged edit
#   - a message naming a symbol absent from both trees blocks (§11.1);
#     one naming a real symbol, a CLI verb and a file does not
#   - the heredoc `-m "$(cat <<'EOF' … EOF)"` message form is recovered
#   - SOVEREIGN_NO_COMMIT_SMELLS=1 silences everything
#   - a clean, traced change with an honest message is silent
#
# Needs only git + python3 (no daemon, no sovereign binary).
#   bash .claude/hooks/tests/commit-smells.sh
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

HOOK="$PWD/.claude/hooks/commit-smells.py"
export CO_ARCH_SCRIPT="$PWD/scripts/co-arch.py"
export CO_ARCH_PROFILE="$PWD/quality/arch-probes.toml"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
export SOVEREIGN_SESSIONS_DIR="$ROOT/sessions"
export TMPDIR="$ROOT/tmp"
mkdir -p "$TMPDIR"
REPO="$ROOT/repo"
export CLAUDE_PROJECT_DIR="$REPO"
unset CO_ARCH_REPO SOVEREIGN_NO_COMMIT_SMELLS

mkdir -p "$REPO/src"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@t.invalid
git -C "$REPO" config user.name t
cat > "$REPO/src/clean.rs" <<'RS'
pub fn run_thing(n: u32) -> Result<u32, Error> {
    let v = n.checked_mul(2).ok_or(Error::Overflow)?;
    tracing::debug!(?v, "doubled");
    Ok(v)
}
RS
git -C "$REPO" add -A && git -C "$REPO" commit -qm "init"

pass=0; fail=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1"; pass=$((pass+1));
          else echo "  FAIL $1: expected [$2] got [$3]"; fail=$((fail+1)); fi; }
has()   { case "$2" in *"$3"*) check "$1" yes yes ;; *) check "$1" "contains '$3'" "absent — $(printf '%s' "$2" | head -c 160)";; esac; }
lacks() { case "$2" in *"$3"*) check "$1" "no '$3'" "present";; *) check "$1" yes yes ;; esac; }

# run_hook <session> <command>  -> RC OUT ERR
run_hook() {
    local sid="$1" cmd="$2"
    OUT="$(python3 -c '
import json, sys
print(json.dumps({"session_id": sys.argv[1], "tool_name": "Bash", "cwd": sys.argv[3],
                  "hook_event_name": "PreToolUse", "tool_input": {"command": sys.argv[2]}}))' \
        "$sid" "$cmd" "$REPO" | python3 "$HOOK" 2>"$ROOT/err")"
    RC=$?
    ERR="$(cat "$ROOT/err")"
}
reset_repo() { git -C "$REPO" reset -q --hard && git -C "$REPO" clean -fdq; }

echo "— non-commit call —"
run_hook s1 "ls -la && cargo build -p foo"
check "non-commit: exit 0" 0 "$RC"
check "non-commit: silent" "" "$OUT$ERR"

echo "— staged smell blocks once per session —"
cat > "$REPO/src/smelly.rs" <<'RS'
pub fn pick(kind: &str, cfg: &Cfg) -> u32 {
    let n = cfg.limit.unwrap_or(0);
    match kind {
        "a" => 1,
        "b" => 2,
        "c" => 3,
        "d" => 4,
        _ => n,
    }
}
RS
git -C "$REPO" add src/smelly.rs
t0=$(date +%s%N)
run_hook s1 'git commit -m "add pick"'
t1=$(date +%s%N)
check "smelly: blocks (exit 2)" 2 "$RC"
has   "smelly: names the code-decided stringly row" "$ERR" "stringly"
has   "smelly: names ARCH §2.1" "$ERR" "ARCH §2.1"
has   "smelly: silent-sub question with its site" "$ERR" "silent-sub"
has   "smelly: cites the file" "$ERR" "src/smelly.rs"
has   "smelly: says how to proceed" "$ERR" "Re-issue the same commit"
echo "       (wall $(( (t1 - t0) / 1000000 ))ms for the blocking call)"
run_hook s1 'git commit -m "add pick"'
check "same session, same sites: proceeds (exit 0)" 0 "$RC"
has   "same session: says the sites are still present" "$OUT" "still present"
check "same session: nothing on stderr" "" "$ERR"
run_hook s2 'git commit -m "add pick"'
check "new session: blocks again" 2 "$RC"
export SOVEREIGN_NO_COMMIT_SMELLS=1
run_hook s3 'git commit -m "add pick"'
check "opt-out env: exit 0" 0 "$RC"
check "opt-out env: silent" "" "$OUT$ERR"
unset SOVEREIGN_NO_COMMIT_SMELLS
reset_repo

echo "— git add X && git commit is one call —"
cat > "$REPO/src/added.rs" <<'RS'
pub fn fire(tx: &Sender<u32>) {
    let _ = tx.send(1);
}
RS
run_hook s4 'git add src/added.rs && git commit -m "fire"'
check "replay: blocks on the not-yet-staged file" 2 "$RC"
has   "replay: cites src/added.rs" "$ERR" "src/added.rs"
has   "replay: the let _ = site" "$ERR" "let _ = tx.send(1);"
check "replay: the REAL index is untouched" "" "$(git -C "$REPO" diff --cached --name-only)"
check "replay: temp index removed" "" "$(ls "$TMPDIR")"
reset_repo

echo "— git commit -a picks up the unstaged edit —"
printf '\npub fn more(cfg: &Cfg) -> u32 { cfg.limit.unwrap_or(7) }\n' >> "$REPO/src/clean.rs"
run_hook s5 'git commit -am "more"'
check "-a: blocks" 2 "$RC"
has   "-a: cites src/clean.rs" "$ERR" "src/clean.rs"
check "-a: real index untouched" "" "$(git -C "$REPO" diff --cached --name-only)"
reset_repo

echo "— §11.1: a symbol the message names must exist somewhere —"
printf '\npub fn also(n: u32) -> u32 {\n    tracing::debug!(n, "also");\n    n\n}\n' >> "$REPO/src/clean.rs"
git -C "$REPO" add src/clean.rs
run_hook s6 'git commit -m "wire `no_such_fn_zq()` into `run_thing`"'
check "uncited: blocks" 2 "$RC"
has   "uncited: names the row" "$ERR" "uncited-symbol"
has   "uncited: names the ghost" "$ERR" "no_such_fn_zq()"
lacks "uncited: does not flag the real symbol" "$ERR" "run_thing\`"
run_hook s7 'git commit -m "extend `run_thing`; run `mesh join`; edit `AGENTS.md`; pass `--tighten`"'
check "honest message + clean traced diff: silent, exit 0" 0 "$RC"
check "honest message: no output at all" "" "$OUT$ERR"

echo "— the heredoc message form is recovered —"
CMD=$'git commit -m "$(cat <<\'EOF\'\nfeat: also\n\nnames `no_such_fn_zq()` and says "quoted" things\nEOF\n)"'
run_hook s8 "$CMD"
check "heredoc: blocks on the ghost" 2 "$RC"
has   "heredoc: names it" "$ERR" "no_such_fn_zq()"
lacks "heredoc: message was recovered (no 'not recovered' note)" "$ERR" "not recovered"
reset_repo

echo "— -F file: read whole, prose mentioning \$( is not a substitution —"
printf '\npub fn more2(n: u32) -> u32 {\n    tracing::debug!(n, "more2");\n    n\n}\n' >> "$REPO/src/clean.rs"
git -C "$REPO" add src/clean.rs
printf 'feat: x\n\nthe `-m "$(cat <<EOF … EOF)"` form, and `no_such_fn_zq()`\n' > "$ROOT/msg.txt"
run_hook s9 "git commit -F $ROOT/msg.txt"
check "-F: blocks on the ghost" 2 "$RC"
has   "-F: names it" "$ERR" "no_such_fn_zq()"
lacks "-F: message was recovered" "$ERR" "not recovered"
run_hook s10 'git commit -m "$(cat notes.txt)"'
check "-m with a real substitution: exit 0 (clean diff)" 0 "$RC"
has   "-m with a real substitution: the skipped check is named" "$OUT" "not recovered"
reset_repo

echo "— envelope without a session id is advisory, never blocking —"
printf '\npub fn again(cfg: &Cfg) -> u32 { cfg.limit.unwrap_or(9) }\n' >> "$REPO/src/clean.rs"
OUT="$(printf '{"tool_name":"Bash","cwd":"%s","tool_input":{"command":"git commit -am x"}}' "$REPO" \
       | python3 "$HOOK" 2>"$ROOT/err")"; RC=$?
check "no session id: exit 0" 0 "$RC"
has   "no session id: says it is advisory" "$OUT" "advisory"

echo
echo "commit-smells: pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
