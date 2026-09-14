#!/bin/bash
# canon-actor-guard.py end to end: the REAL hook against the command shapes an
# agent actually types. The refusals are the gate watched failing (ARCH 5);
# the allows are the false positives that would teach an agent to route around
# it — a commit message quoting a verb, a reader of the ledger, a draft.
#
# Needs only python3.
#   bash .claude/hooks/tests/canon-actor-guard.sh
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

HOOK="$PWD/.claude/hooks/canon-actor-guard.py"
unset SOVEREIGN_HOOK_INPUT
pass=0; fail=0

payload() {
    python3 -c 'import json, sys
tool, value = sys.argv[1], sys.argv[2]
key = "command" if tool == "Bash" else "file_path"
print(json.dumps({"tool_name": tool, "tool_input": {key: value}}))' "$1" "$2"
}

# check <refuse|allow|skip> <actor, or - for unset> <tool> <input> [label]
check() {
    local want=$1 actor=$2 tool=$3 input=$4 label=${5:-$4}
    local err out rc got
    err=$(mktemp)
    if [ "$actor" = "-" ]; then
        out=$(payload "$tool" "$input" | env -u CANON_ACTOR python3 "$HOOK" 2>"$err"); rc=$?
    else
        out=$(payload "$tool" "$input" | CANON_ACTOR="$actor" python3 "$HOOK" 2>"$err"); rc=$?
    fi
    if [ $rc -eq 2 ] && [ -s "$err" ]; then got=refuse
    elif [ $rc -eq 0 ] && [ ! -s "$err" ] && [ -z "$out" ]; then got=allow
    elif [ $rc -eq 0 ] && grep -q 'canon-actor-guard skipped' <<<"$out"; then got=skip
    else got="rc=$rc"; fi
    if [ "$got" = "$want" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "  FAIL want $want, got $got: $label"
        sed 's/^/      /' "$err"
    fi
    rm -f "$err"
}

A=agent:claude-code

echo "adjudication verbs are refused under any actor"
check refuse $A Bash 'canon approve c1a2 -m ok'
check refuse $A Bash 'canon draft --resume'
check refuse $A Bash 'canon draft --resume 1789319124'
check refuse $A Bash 'canon grant "human:Alex Bryan" commonwealth'
check refuse $A Bash 'canon policy set standing -m x'
check refuse $A Bash 'canon object c1a2 -m "no"'
check refuse $A Bash 'canon frobnicate' 'an unclassed verb fails closed'
check refuse $A Bash 'cd /tmp && ~/dev/canon/target/debug/canon supersede c1 "new" -m why' 'by path, after &&'
check refuse $A Bash 'bash -c "canon approve c1"' 'inside bash -c'
check refuse $A Bash 'echo $(canon retract c1 -m x)' 'unquoted command substitution'
check refuse $A Bash $'ls\ncanon accept a b -m x' 'on a later line'
check refuse $A Bash $'cat > /tmp/x <<EOF\nhello\nEOF\ncanon dismiss a b' 'after a heredoc'

echo "propose verbs need an agent: actor"
check refuse - Bash 'canon add "a rule"' 'unset actor falls back to the git name'
check refuse $A Bash 'CANON_ACTOR="human:Alex Bryan" canon add "a rule"' 'prefix override'
check refuse $A Bash 'env -u CANON_ACTOR canon question "q"' 'env -u'
check refuse $A Bash 'env -i PATH=/bin canon add x' 'env -i'
check refuse $A Bash 'export CANON_ACTOR=human:x; canon add y' 'export earlier in the command'
check refuse $A Bash 'unset CANON_ACTOR && canon add y' 'unset earlier in the command'
check refuse $A Bash 'CANON_ACTOR=human:x; canon add y' 'bare assignment earlier'
check allow $A Bash 'canon add "a rule" --scope commonwealth'
check allow $A Bash 'canon draft --from .canon/sources/notes --yes </dev/null'
check allow $A Bash 'CANON_DIR=target/canon-staging/notes/.canon canon draft --from .canon/sources/notes --yes'

echo "reads are allowed under any actor"
check allow - Bash 'canon list'
check allow - Bash 'canon --json why c1a2'
check allow - Bash 'canon log | tail -5'
check allow - Bash 'canon check "move notes into canon"'
check allow - Bash 'canon policy show'
check allow - Bash 'canon draft --from - --dry-run --json' 'dry-run draft writes nothing'
check allow - Bash 'canon approve --help'
check allow - Bash 'canon'

echo "quoted verbs are data, not invocations"
check allow - Bash 'git commit -m "canon approve is the operator'"'"'s act"'
check allow - Bash $'git commit -m "$(cat <<\'EOF\'\nfix: guard\n\ncanon approve c1 -m ok\nEOF\n)"' 'heredoc commit body'
check allow - Bash 'echo canon approve c1'
check allow - Bash '# canon approve c1'

echo "the ledger is appended by canon alone"
check refuse $A Bash "echo '{}' >> .canon/acts.jsonl"
check refuse $A Bash 'python3 fix.py .canon/acts.jsonl'
check refuse $A Bash 'git checkout -- .canon/acts.jsonl'
check refuse $A Edit '/Users/x/dev/commonwealth-ai/.canon/acts.jsonl'
check refuse $A Write '.canon/draft-runs/1789319124.json' 'hand-forged draft run'
check allow $A Bash 'wc -l .canon/acts.jsonl && grep -c approve .canon/acts.jsonl'
check allow $A Bash 'python3 summarize.py < .canon/acts.jsonl'
check allow $A Bash 'git add .canon/acts.jsonl && git -C . log --oneline -- .canon/acts.jsonl'
check allow $A Bash 'mv target/canon-staging/notes/.canon/draft-runs/1.json .canon/draft-runs/' 'moving a finished run is not forging one'
check allow $A Write '.canon/sources/notes/n1.md'

echo "everything else passes through"
check allow - Read '.canon/acts.jsonl'
check skip - Bash 'echo "unterminated' 'an unlexable command names the skip'

echo "the harness-neutral envelope (\$SOVEREIGN_HOOK_INPUT)"
err=$(mktemp)
SOVEREIGN_HOOK_INPUT="$(payload Bash 'canon approve c1')" CANON_ACTOR=$A python3 "$HOOK" </dev/null 2>"$err"
rc=$?
if [ $rc -eq 2 ]; then pass=$((pass + 1)); else fail=$((fail + 1)); echo "  FAIL envelope via env: rc=$rc"; fi
rm -f "$err"

echo "canon-actor-guard: pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
