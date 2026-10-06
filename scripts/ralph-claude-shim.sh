#!/usr/bin/env bash
# ralph worker on the Claude subscription instead of opencode+openrouter.
#
# scripts/ralph.py spawns `$RALPH_OPENCODE_BIN run [--model M] [--variant V] <prompt>`
# and reads back only the session's `ralph-result` and whether a commit
# landed. It was hardwired to opencode, whose only key on this host was
# openrouter (2026-09-17: $40 gone in a day and a half, the last three
# iterations dying on "would exceed your available credits"). This shim keeps
# ralph.py untouched and maps that call onto `claude -p`, billed to the
# logged-in claude.ai plan:
#
#   RALPH_OPENCODE_BIN=$PWD/scripts/ralph-claude-shim.sh python3 scripts/ralph.py supervise ...
#
# Permissions are NOT bypassed. The worker runs under ralph/claude-settings.json
# (--settings): acceptEdits for file edits inside the repo, an explicit Bash
# allowlist drawn from the queue's check table, and a deny list for the
# prompt's hard rules. In print mode nothing can prompt, so a call outside the
# list is refused; the renderer below prints each refusal as an
# `auto-rejecting` line, which is the literal ralph.py counts to raise its
# "permission auto-rejections" warning. Extend the settings file, not this.
#
# Also on purpose:
#   - --variant maps to claude's --effort when it names an effort level; any
#     other value is opencode-only and is dropped, with one line on stderr (the iter log) saying so.
#   - NOT --bare: --bare skips the credential read and reports "Not logged in".
#   - The nested-session vars are unset so the worker is a root session, not
#     a child of whichever Claude Code seat launched the loop.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
settings="${RALPH_CLAUDE_SETTINGS:-$here/ralph/claude-settings.json}"

verb="${1:-}"; shift || true
[ "$verb" = "run" ] || { echo "ralph-claude-shim: only 'run' is supported, got '$verb'" >&2; exit 2; }

model=""; effort=""; dropped=""
while [ $# -gt 1 ]; do
    case "$1" in
        --model)   model="$2"; shift 2 ;;
        # The manifest's `variant` is the queue's effort knob. claude -p spells
        # it --effort and accepts exactly these levels; any other value is an
        # opencode-only variant and is dropped, said once on stderr (the iter log).
        --variant) case "$2" in
                       low|medium|high|xhigh|max) effort="$2" ;;
                       *) [ -n "$dropped" ] || echo "ralph-claude-shim: dropping --variant $2 (not a claude effort level: low|medium|high|xhigh|max) — this session runs at the model's default effort" >&2
                          dropped=1 ;;
                   esac
                   shift 2 ;;
        *) echo "ralph-claude-shim: unknown flag $1" >&2; exit 2 ;;
    esac
done
prompt="${1:-}"
[ -n "$prompt" ] || { echo "ralph-claude-shim: empty prompt" >&2; exit 2; }
[ -f "$settings" ] || { echo "ralph-claude-shim: no settings file at $settings" >&2; exit 2; }

args=(-p --settings "$settings" --output-format stream-json --verbose)
[ -n "$model" ] && args+=(--model "$model")
[ -n "$effort" ] && args+=(--effort "$effort")
# claude's init event names the model but not the effort, so the iter log gets both from here.
echo "ralph-claude-shim: model=${model:-default} effort=${effort:-default}" >&2
# A print-mode session exits the moment the model ends its turn, and its
# background children die with it. Watched 2026-09-18 00:39Z: the worker
# backgrounded `ralph-check.sh demo` (longer than the Bash tool's 2 min
# default), said "when it finishes I'll paste the rows", ended its turn 36 s
# in, and the next session repeated it - a stall that would have halted the
# loop. This is the harness's property, so it is said here, not in PROMPT.md.
args+=(--append-system-prompt "HARNESS: you are a one-shot print-mode session. The process exits the moment you end your turn, and every background task you started is killed with it - no notification will ever reach you. Never run a command in the background and never end your turn while a check is running. Run long checks (DEMO, TEST, LINT, TESTALL, PREPUSH, anything under the cargo lock) in the FOREGROUND with the Bash tool's timeout parameter set to 600000; if a single check cannot finish inside ten minutes, split it, or end your turn with: ralph-result await <budget> -- <the check> (the loop runs it and resumes you with its exit code and log). Commit before you end your turn - an uncommitted turn is lost.")
# The directories .opencode/opencode.json's external_directory already
# granted: /run (the PROMPT's containerenv premise check), /tmp (the cargo
# lock), and ralph's own state under ~/.svrnmesh. Anything else outside the
# repo still asks.
for d in /run /tmp "$HOME/.svrnmesh/ralph"; do
    [ -d "$d" ] && args+=(--add-dir "$d")
done

# The gray zone — a call the settings neither allow nor deny — bubbles to the
# operator through scripts/ralph-permission-bridge.py (PERMISSION_REQUEST.md in
# the loop's control dir, answered by PERMISSION_ANSWER beside it, denied after
# RALPH_PERMISSION_WAIT_SECS). RALPH_SESSION_CWD tells it which pool lane asks.
export RALPH_SESSION_CWD="$PWD"
# RALPH_PERMISSION_BRIDGE=0 falls back to refusing the gray zone outright.
#
# MCP is pinned: the repo's .mcp.json (code intel) plus the bridge, and
# nothing from the user's own config — a worker has no business with web
# search or the claude.ai connectors the seat happens to have.
mcp_json="{\"mcpServers\":{\"ralph\":{\"command\":\"python3\",\"args\":[\"$here/scripts/ralph-permission-bridge.py\"]}}}"
args+=(--strict-mcp-config --mcp-config "$here/.mcp.json")
if [ "${RALPH_PERMISSION_BRIDGE:-1}" != "0" ]; then
    args+=(--mcp-config "$mcp_json" --permission-prompts host --permission-prompt-tool mcp__ralph__approve)
fi

# stream-json -> a readable iter log via scripts/ralph-claude-render.py.
printf '%s' "$prompt" \
| env -u CLAUDECODE -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SESSION_ID \
      -u CLAUDE_CODE_MESSAGING_SOCKET -u CLAUDE_CODE_MESSAGING_TOKEN \
      claude "${args[@]}" \
| python3 -u "$here/scripts/ralph-claude-render.py"
exit "${PIPESTATUS[1]}"
