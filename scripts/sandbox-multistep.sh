#!/usr/bin/env bash
# Ceteris-paribus multi-step coding sandbox.
#
# Holds these constant vs the codex+superpowers smoke:
#   - model      (Qwen3.6-35B-A3B-UD-MTP-IQ4_NL)
#   - backend    (commonwealth daemon /v1/chat/completions)
#   - task       (impl Capability enum + round-trip test + cargo test)
#
# Varies ONLY:
#   - harness    (this script vs codex)
#   - tool menu  (4 tools, all relevant vs codex's 11 + 2 synthetic)
#   - protocol   (chat.completions tool envelope vs Responses API)
#
# If the model completes the task here but bombed via codex, the
# bottleneck is harness/protocol — not model capability or backend.
# If it bombs here too, the model genuinely can't sustain multi-step
# coding agency on this task.
#
# Usage: scripts/sandbox-multistep.sh [max_turns]

set -u

MODEL="${MODEL:-Qwen3.6-35B-A3B-UD-MTP-IQ4_NL}"
DAEMON="${DAEMON:-http://localhost:9741}"
MAX_TURNS="${1:-30}"
WORKDIR=/tmp/sandbox-multistep-$$
LOG="$WORKDIR/turns.log"

mkdir -p "$WORKDIR/src" "$WORKDIR/tests"
cat > "$WORKDIR/Cargo.toml" <<'EOF'
[package]
name = "sandbox-cap"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
EOF
cat > "$WORKDIR/src/lib.rs" <<'EOF'
//! sandbox-cap — Capability enum live here.
EOF

SYSTEM='You complete a coding task by emitting tool calls. One tool per turn. After each call you get the tool result back as a user message, then you decide the next call.

Tools (pick exactly one per turn):
- write_file(path, content): create or replace a file. content is the verbatim file body.
- read_file(path): read a file. result is the contents.
- bash(cmd): run a shell command. result is "exit=<n>\nstdout=<...>\nstderr=<...>".
- done(reason): the task is complete and verified.

Output format — exactly ONE JSON object per turn, no prose, no markdown fences:
  {"tool":"<name>","args":{...}}

Examples:
  {"tool":"read_file","args":{"path":"/tmp/x/src/lib.rs"}}
  {"tool":"write_file","args":{"path":"/tmp/x/src/lib.rs","content":"pub fn hi() {}\n"}}
  {"tool":"bash","args":{"cmd":"cd /tmp/x && cargo test"}}
  {"tool":"done","args":{"reason":"cargo test passes; 1 test passed."}}

If a tool fails or returns unexpected output, fix and retry. Do not call done until the verifying command passes.'

TASK="Implement a Rust crate at $WORKDIR.

Files to write:
  $WORKDIR/src/lib.rs  -- the Capability enum
  $WORKDIR/tests/test_capability.rs -- one round-trip test for the Unknown variant

The Capability enum:
- variants: General, Code, Extension(String), Unknown(String)
- derive Serialize, Deserialize, Debug, PartialEq
- use #[serde(rename_all = \"snake_case\")] so wire form is \"general\", \"code\", etc.
- Extension serializes as the inner string (use #[serde(untagged)] OR a per-variant attribute)
- Unknown is a #[serde(other)] catch-all so any unknown wire string deserializes to Unknown — but #[serde(other)] only works on unit variants, so use a custom Deserialize impl, OR a top-level untagged enum. Pick whichever approach you prefer; the test just has to pass.

The test:
- function name: roundtrip_unknown_capability
- fixture: \\\"math\\\" deserializes to Capability::Unknown(\\\"math\\\".to_string())  (OR any equivalent representation you've chosen — the assertion is yours; the test must demonstrate that an unknown wire value parses to the Unknown variant without erroring)
- runs via: cargo test --test test_capability -- roundtrip_unknown_capability

Stop condition: \\\"cargo test\\\" inside the crate directory exits 0 with at least one test passing.

Begin."

# Build the initial messages array as a JSON file we'll mutate each turn.
MSGS_FILE="$WORKDIR/messages.json"
python3 - <<PY > "$MSGS_FILE"
import json, sys, os
print(json.dumps([
  {"role":"system","content":${SYSTEM_JSON:-os.environ.get('SYSTEM', '')}},
  {"role":"user","content":${TASK_JSON:-os.environ.get('TASK','')}}
]))
PY

# Re-emit cleanly via jq.
jq -n --arg s "$SYSTEM" --arg u "$TASK" \
  '[{"role":"system","content":$s},{"role":"user","content":$u}]' > "$MSGS_FILE"

echo "workdir: $WORKDIR" | tee "$LOG"
echo "model:   $MODEL"   | tee -a "$LOG"
echo "daemon:  $DAEMON"  | tee -a "$LOG"
echo "max_turns: $MAX_TURNS" | tee -a "$LOG"
echo "" | tee -a "$LOG"

for turn in $(seq 1 "$MAX_TURNS"); do
  echo "=== turn $turn ===" | tee -a "$LOG"

  REQ=$(jq -n --arg m "$MODEL" --argjson msgs "$(cat "$MSGS_FILE")" \
    '{model:$m, messages:$msgs, temperature:0, max_tokens:4096}')

  RESP=$(curl -sf "$DAEMON/v1/chat/completions" \
    -X POST -H 'content-type: application/json' -d "$REQ" \
    --max-time 180 2>>"$LOG")
  if [ -z "$RESP" ]; then
    echo "ABORT: empty response from daemon" | tee -a "$LOG"
    break
  fi
  TEXT=$(echo "$RESP" | jq -r '.choices[0].message.content // ""')
  FINISH=$(echo "$RESP" | jq -r '.choices[0].finish_reason // "?"')
  echo "model finish_reason=$FINISH" | tee -a "$LOG"
  echo "model raw: $TEXT" | tee -a "$LOG"

  # Strip any <think>...</think> block the model emitted before the JSON.
  STRIPPED=$(printf '%s' "$TEXT" | python3 -c "
import sys, re
t = sys.stdin.read()
t = re.sub(r'<think>.*?</think>', '', t, flags=re.DOTALL)
print(t.strip())
")

  # Locate the first balanced JSON object in the response.
  CALL=$(printf '%s' "$STRIPPED" | python3 -c "
import sys, json
t = sys.stdin.read()
start = t.find('{')
if start < 0:
    sys.exit(1)
depth = 0; in_str = False; esc = False
end = -1
for i, ch in enumerate(t[start:], start):
    if esc: esc = False; continue
    if ch == '\\\\': esc = True; continue
    if ch == '\"': in_str = not in_str; continue
    if in_str: continue
    if ch == '{': depth += 1
    elif ch == '}':
        depth -= 1
        if depth == 0: end = i+1; break
if end < 0:
    sys.exit(1)
try:
    obj = json.loads(t[start:end])
    print(json.dumps(obj))
except Exception as e:
    sys.exit(1)
")
  if [ -z "$CALL" ]; then
    echo "ABORT: could not parse tool call from model output" | tee -a "$LOG"
    break
  fi
  echo "tool call: $CALL" | tee -a "$LOG"

  TOOL=$(echo "$CALL" | jq -r '.tool // .name // ""')
  ARGS=$(echo "$CALL" | jq -c '.args // .arguments // {}')

  case "$TOOL" in
    write_file)
      PATH_ARG=$(echo "$ARGS" | jq -r '.path // ""')
      CONTENT=$(echo "$ARGS" | jq -r '.content // ""')
      if [ -z "$PATH_ARG" ]; then
        RESULT='{"error":"path required"}'
      else
        mkdir -p "$(dirname "$PATH_ARG")"
        printf '%s' "$CONTENT" > "$PATH_ARG"
        BYTES=$(wc -c < "$PATH_ARG" | tr -d ' ')
        RESULT="{\"ok\":true,\"path\":\"$PATH_ARG\",\"bytes_written\":$BYTES}"
      fi
      ;;
    read_file)
      PATH_ARG=$(echo "$ARGS" | jq -r '.path // ""')
      if [ -z "$PATH_ARG" ] || [ ! -f "$PATH_ARG" ]; then
        RESULT="{\"error\":\"file not found: $PATH_ARG\"}"
      else
        BODY=$(cat "$PATH_ARG")
        RESULT=$(jq -n --arg b "$BODY" '{ok:true, content:$b}')
      fi
      ;;
    bash)
      CMD=$(echo "$ARGS" | jq -r '.cmd // .command // ""')
      if [ -z "$CMD" ]; then
        RESULT='{"error":"cmd required"}'
      else
        STDOUT=$(/bin/zsh -lc "$CMD" 2>"$WORKDIR/_stderr")
        EXIT=$?
        STDERR=$(cat "$WORKDIR/_stderr")
        RESULT=$(jq -n --arg o "$STDOUT" --arg e "$STDERR" --argjson x "$EXIT" \
          '{ok:($x==0), exit:$x, stdout:$o, stderr:$e}')
      fi
      ;;
    done)
      REASON=$(echo "$ARGS" | jq -r '.reason // ""')
      echo "=== DONE turn=$turn reason=$REASON ===" | tee -a "$LOG"
      echo "RUNNING FINAL cargo test --quiet IN $WORKDIR..." | tee -a "$LOG"
      cd "$WORKDIR" && cargo test --quiet 2>&1 | tee -a "$LOG"
      cd - >/dev/null
      exit 0
      ;;
    *)
      RESULT="{\"error\":\"unknown tool: $TOOL\"}"
      ;;
  esac

  echo "tool result: $(echo "$RESULT" | head -c 240)..." | tee -a "$LOG"

  # Append assistant + tool-result messages to history.
  jq --arg ass "$TEXT" --arg res "$RESULT" \
    '. + [{"role":"assistant","content":$ass},{"role":"user","content":$res}]' \
    "$MSGS_FILE" > "$MSGS_FILE.new" && mv "$MSGS_FILE.new" "$MSGS_FILE"
done

echo "=== EXHAUSTED max_turns=$MAX_TURNS without done call ===" | tee -a "$LOG"
echo "WORKDIR: $WORKDIR"
exit 2
