#!/bin/bash
# launchagent-guard.py end to end: the REAL hook against the shapes an agent
# types to arm a launchd job. The refusals are the gate watched failing
# (ARCH 5); the allows are the false positives that would teach an agent to
# route around it: retiring a plist, restarting the daemon, a commit message
# that quotes the bad command.
#
# HOME points at a temp dir holding one existing agent, so "would this create
# a plist" is decided against a known directory, never the real one.
#
# Needs only python3.
#   bash .claude/hooks/tests/launchagent-guard.sh
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

HOOK="$PWD/.claude/hooks/launchagent-guard.py"
unset SOVEREIGN_HOOK_INPUT
FAKE_HOME=$(mktemp -d)
trap 'rm -rf "$FAKE_HOME"' EXIT
mkdir -p "$FAKE_HOME/Library/LaunchAgents"
touch "$FAKE_HOME/Library/LaunchAgents/com.svrnmesh.daemon.plist" \
      "$FAKE_HOME/Library/LaunchAgents/ai.old.plist"
pass=0; fail=0

payload() {
    python3 -c 'import json, sys
tool, value = sys.argv[1], sys.argv[2]
key = "command" if tool == "Bash" else "file_path"
print(json.dumps({"tool_name": tool, "tool_input": {key: value}}))' "$1" "$2"
}

verdict() {   # verdict <rc> <stdout> <stderr-file>
    if [ "$1" -eq 2 ] && [ -s "$3" ]; then echo refuse
    elif [ "$1" -eq 0 ] && [ ! -s "$3" ] && [ -z "$2" ]; then echo allow
    elif [ "$1" -eq 0 ] && grep -q 'launchagent-guard skipped' <<<"$2"; then echo skip
    else echo "rc=$1"; fi
}

# check <refuse|allow|skip> <tool> <input> [label]
check() {
    local want=$1 tool=$2 input=$3 label=${4:-$3}
    local err out rc got
    err=$(mktemp)
    out=$(payload "$tool" "$input" | env -u LABEL HOME="$FAKE_HOME" python3 "$HOOK" 2>"$err"); rc=$?
    got=$(verdict "$rc" "$out" "$err")
    if [ "$got" = "$want" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "  FAIL want $want, got $got: $label"
        sed 's/^/      /' "$err"
    fi
    rm -f "$err"
}

H=$FAKE_HOME

echo "creating a plist in LaunchAgents is refused"
check refuse Write "$H/Library/LaunchAgents/ai.new.plist" 'Write, absolute'
check refuse Write '~/Library/LaunchAgents/ai.new.plist' 'Write, tilde'
check refuse Bash $'cat > ~/Library/LaunchAgents/ai.new.plist <<\'EOF\'\n<plist/>\nEOF' 'heredoc into a redirect'
check refuse Bash $'cat >"$HOME/Library/LaunchAgents/ai.new.plist" <<EOF\n<plist/>\nEOF' 'redirect with no space, $HOME'
check refuse Bash 'printf "%s" "$x" | tee -a ~/Library/LaunchAgents/ai.new.plist >/dev/null' 'tee'
check refuse Bash 'cp runs/r/ai.new.plist ~/Library/LaunchAgents/ 2>/dev/null && launchctl bootstrap gui/501 ~/Library/LaunchAgents/ai.new.plist' 'cp into the dir, 2> after it'
check refuse Bash 'mv runs/r/ai.new.plist "$HOME/Library/LaunchAgents"' 'mv into the dir, no slash'
check refuse Bash $'P=~/Library/LaunchAgents/ai.new.plist; cat > "$P" <<EOF\nx\nEOF' 'through a shell variable'
check refuse Bash 'ln -s /tmp/ai.new.plist ~/Library/LaunchAgents/ai.new.plist' 'ln -s'
check refuse Bash 'install -m 644 /tmp/ai.new.plist ~/Library/LaunchAgents/ai.new.plist' 'install -m'
check refuse Bash 'bash -c "cp /tmp/ai.new.plist ~/Library/LaunchAgents/"' 'inside bash -c'
check refuse Bash $'ls\ncp /tmp/ai.new.plist ~/Library/LaunchAgents/' 'on a later line'
check refuse Bash 'cat > ~/Library/LaunchAgents/$LABEL.plist < x.plist' 'an unset variable is a new name'

echo "everything else passes through"
check allow Write /tmp/scratch/ai.new.plist 'Write elsewhere'
check allow Write "$H/.svrnmesh/oneshot/ai.new.plist" 'Write under the state dir'
check allow Write "$H/Library/LaunchAgents/com.svrnmesh.daemon.plist" 'rewriting an existing agent'
check allow Edit "$H/Library/LaunchAgents/ai.new.plist" 'Edit cannot create a file'
check allow Bash 'launchctl bootstrap gui/501 ~/Library/LaunchAgents/com.svrnmesh.daemon.plist' 'restarting the daemon'
check allow Bash 'launchctl bootstrap gui/$(id -u) runs/r/ai.new.plist' 'the remedy itself'
check allow Bash 'cat ~/Library/LaunchAgents/com.svrnmesh.daemon.plist' 'a reader'
check allow Bash 'ls ~/Library/LaunchAgents/ > /tmp/agents.txt' 'listing to a file elsewhere'
check allow Bash 'launchctl bootout gui/501/ai.old; mv ~/Library/LaunchAgents/ai.old.plist ~/.svrnmesh/retired/' 'retiring an agent'
check allow Bash 'rm ~/Library/LaunchAgents/ai.old.plist' 'rm'
check allow Bash $'cat > ~/Library/LaunchAgents/com.svrnmesh.daemon.plist <<EOF\nx\nEOF' 'redirect onto an existing agent'
check allow Bash $'git commit -F - <<\'EOF\'\nfix: never cp x.plist ~/Library/LaunchAgents/\nEOF' 'a commit message quoting it'
check allow Bash 'git commit -m "never cp x.plist ~/Library/LaunchAgents/"' 'quoted inside -m'
check allow Bash 'cp a.plist b.plist 2>/dev/null' 'an unrelated copy'
check skip Bash 'cp "unterminated ~/Library/LaunchAgents/' 'a command that will not lex'

echo "the harness-neutral envelope (\$SOVEREIGN_HOOK_INPUT)"
err=$(mktemp)
SOVEREIGN_HOOK_INPUT=$(payload Bash 'cp /tmp/ai.new.plist ~/Library/LaunchAgents/') \
    HOME="$FAKE_HOME" python3 "$HOOK" </dev/null 2>"$err"; rc=$?
got=$(verdict "$rc" "" "$err")
if [ "$got" = refuse ]; then pass=$((pass + 1)); else fail=$((fail + 1)); echo "  FAIL want refuse, got $got: envelope"; fi
grep -q 'launchctl bootstrap gui/\$(id -u) <dir>/ai.new.plist' "$err" \
    && pass=$((pass + 1)) \
    || { fail=$((fail + 1)); echo "  FAIL the refusal does not name the remedy:"; sed 's/^/      /' "$err"; }
rm -f "$err"

echo "launchagent-guard: pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
