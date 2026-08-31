#!/bin/sh
# Format a file the agent just wrote — style is applied, never argued about.
#
# Harness-neutral, like every script in this directory: it reads the tool-call
# envelope on stdin (or $SOVEREIGN_HOOK_INPUT) and needs nothing from the
# harness but the edited path. Claude Code wires it as a PostToolUse hook on
# Write|Edit in .claude/settings.json; pi's adapter calls it the same way.
#
# WHY THIS EXISTS. rustfmt was enforced in exactly two places, both of them
# late: `run_gate "rustfmt"` in scripts/pre-push.sh, and the blocking `fmt` job
# in CI. Neither one formats anything — they report that formatting is wrong,
# minutes or hours after the code was written, and the fix is always the same
# mechanical command. `scripts/sovereign-lint.sh` never covered it at all, so
# the repo's own definition of done could pass on unformatted code. Applying
# the formatter at the moment of the edit removes the class: the two gates stay
# as the backstop for edits this hook never sees (human edits, other machines,
# `--no-verify`), and they should now essentially never fire.
#
# COVERAGE. Claude Code only, today. This script is harness-neutral, but pi's
# adapter (.pi/extensions/sovereign-hooks/index.ts) maps just `session_start`
# and `before_agent_start` — there is no post-tool mapping, so a pi session
# gets no auto-formatting until someone adds one. Do NOT add a fmt step to
# `scripts/sovereign-lint.sh` to cover the gap: that turns a fix back into a
# gate, which is the thing this replaces. `run_gate "rustfmt"` in
# scripts/pre-push.sh and CI's blocking `fmt` job are already the backstop for
# every edit this hook does not see.
#
# CONTRACT: this hook NEVER blocks. It exits 0 whatever happens, because a
# formatter is not a correctness check and a half-written file that rustfmt
# cannot parse is a normal intermediate state, not an error worth interrupting
# for. The one thing it will not do is stay quiet about being unable to run at
# all — see the missing-rustfmt branch.

set -u

payload="${SOVEREIGN_HOOK_INPUT:-}"
[ -n "$payload" ] || payload="$(cat)"

file="$(printf '%s' "$payload" \
    | jq -r '.tool_input.file_path // .tool_response.filePath // empty' 2>/dev/null)"
[ -n "${file:-}" ] || exit 0
[ -f "$file" ] || exit 0

# A closed set, spelled as one: add a branch per language whose formatter is
# deterministic and safe to apply unattended. Anything not listed is left
# alone rather than guessed at.
case "$file" in
    *.rs) ;;
    *) exit 0 ;;
esac

if ! command -v rustfmt >/dev/null 2>&1; then
    # Report absence, never default around it. Silence here would look exactly
    # like "your code is formatted", which is the failure this hook prevents.
    printf '%s\n' '{"systemMessage":"rustfmt is not on PATH — Rust files are NOT being auto-formatted; `cargo fmt --all --check` will fail in CI. Install it with `rustup component add rustfmt`."}'
    exit 0
fi

# rustfmt.toml is the single home for formatting policy (its own header says
# so), including `edition`. Read the edition from it rather than hardcoding
# one here: a bare `--edition` default of 2015 silently mangles `async`/`dyn`,
# and a hardcoded 2021 would go stale and OVERRIDE the config the day the
# workspace moves editions, since the CLI flag beats the config file.
dir="$(CDPATH= cd -- "$(dirname -- "$file")" && pwd)"
edition=""
while [ -n "$dir" ]; do
    for cfg in "$dir/rustfmt.toml" "$dir/.rustfmt.toml"; do
        if [ -f "$cfg" ]; then
            edition="$(sed -n 's/^[[:space:]]*edition[[:space:]]*=[[:space:]]*"\([0-9]\{4\}\)".*/\1/p' "$cfg" | head -n1)"
            [ -n "$edition" ] && break
        fi
    done
    [ -n "$edition" ] && break
    [ "$dir" = "/" ] && break
    dir="$(dirname -- "$dir")"
done
[ -n "$edition" ] || edition=2021

# Failure is deliberately silent: the usual cause is a file that does not parse
# yet, which the compiler will report far better than this hook could.
rustfmt --edition "$edition" -- "$file" >/dev/null 2>&1

exit 0
