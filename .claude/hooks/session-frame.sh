#!/bin/sh
# session-frame.sh — externalize the session frame at harness lifecycle
# moments (PreCompact / SessionEnd). SESSION_CONTINUITY.md §3 write-path 2:
# the frame must exist even when the agent never self-reported — never
# rely on model discipline for anything load-bearing.
#
# Receives the hook envelope on stdin (session_id, transcript_path,
# hook_event_name, cwd). Kicks off `sovereign session distill` fully
# DETACHED and exits immediately: distillation runs an LLM call against
# the local daemon (seconds to a minute), and neither compaction nor
# session exit should wait on it. This is safe because the transcript
# JSONL retains full history regardless of compaction — a frame written
# a minute late is still a correct frame.
#
# Degrade path (spec: degrade honestly, never silently): if the full
# distill fails (daemon down), retry with --no-llm so at least the
# deterministic spine lands. All output goes to the distill log.

envelope=$(cat)

field() {
    printf '%s' "$envelope" | python3 -c "import json,sys; print(json.load(sys.stdin).get('$1') or '')" 2>/dev/null
}

sid=$(field session_id)
tpath=$(field transcript_path)
cwd=$(field cwd)
event=$(field hook_event_name)

[ -n "$sid" ] || exit 0
[ -f "$tpath" ] || exit 0

# Skip sessions with nothing worth framing. A transcript under 100KB is
# a handful of turns — the boot brief alone covers re-orientation, and
# framing it would burn a daemon LLM call for no successor value.
size=$(wc -c < "$tpath" 2>/dev/null | tr -d ' ')
[ "${size:-0}" -ge 100000 ] || exit 0

SOV_BIN=$(command -v sovereign || true)
[ -n "$SOV_BIN" ] || SOV_BIN="$HOME/.local/bin/sovereign"
[ -x "$SOV_BIN" ] || exit 0

frame_dir="$HOME/.sovereign/sessions/$sid"
log="$HOME/.sovereign/sessions/distill-hook.log"
lock="$frame_dir/.distill.lock"
mkdir -p "$frame_dir"

# One distill per session at a time. PreCompact and SessionEnd can fire
# close together; a lock younger than 10 minutes means one is already
# running — the transcript it reads includes everything ours would.
if [ -f "$lock" ] && [ -n "$(find "$lock" -mmin -10 2>/dev/null)" ]; then
    exit 0
fi
touch "$lock"

# Fully detach: setsid where available (Linux), plain nohup on macOS.
# The subshell must survive the harness process exiting on SessionEnd.
(
    printf '%s %s distill start (event=%s size=%s)\n' \
        "$(date '+%Y-%m-%dT%H:%M:%S')" "$sid" "$event" "$size" >> "$log"
    if ! "$SOV_BIN" session distill "$sid" --project "${cwd:-.}" >> "$log" 2>&1; then
        printf '%s %s full distill failed — falling back to --no-llm spine\n' \
            "$(date '+%Y-%m-%dT%H:%M:%S')" "$sid" >> "$log"
        "$SOV_BIN" session distill "$sid" --project "${cwd:-.}" --no-llm >> "$log" 2>&1
    fi
    rm -f "$lock"
) < /dev/null > /dev/null 2>&1 &

exit 0
