#!/usr/bin/env bash
# cli-contract-live-verify.sh — exercise the READ-ONLY smoke probes declared in
# docs/cli-contract.toml against a LIVE daemon. The behavioral half of the CLI
# contract harness; the offline halves are the cli_contract_{docs,code} tests.
#
# Opt-in + self-skipping: does nothing unless SOVEREIGN_LIVE_CONTRACT=1, and
# skips with exit 0 if the daemon at :9741 isn't reachable — so it is safe to
# call unconditionally in CI. Mirrors scripts/phase3-lifecycle-verify.sh.
#
# Safety: every smoke command is checked against a hardcoded READ-ONLY verb
# allowlist before it runs; anything else is refused. Defense in depth on top
# of the manifest's own rule that smoke blocks must be side-effect-free.
set -euo pipefail

# ── locate the dispatcher binary ─────────────────────────────────────────
BIN="${SOVEREIGN_BIN:-}"
if [ -z "$BIN" ]; then
  for c in target/release/sovereign-cli target/debug/sovereign-cli; do
    [ -x "$c" ] && BIN="$c" && break
  done
fi
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
  echo "cli-contract-live: sovereign-cli not built (set SOVEREIGN_BIN, or cargo build -p sovereign-cli)"
  exit 2
fi

# ── opt-in gate ──────────────────────────────────────────────────────────
if [ "${SOVEREIGN_LIVE_CONTRACT:-0}" != "1" ]; then
  echo "cli-contract-live: skipped (set SOVEREIGN_LIVE_CONTRACT=1 to run)"
  exit 0
fi

# ── daemon reachability (skip cleanly if down, unless STRICT) ─────────────
DAEMON="${SOVEREIGN_DAEMON_URL:-http://127.0.0.1:9741}"
if ! curl -fsS -m 3 "$DAEMON/v1/models" >/dev/null 2>&1; then
  if [ "${SOVEREIGN_LIVE_STRICT:-0}" = "1" ]; then
    echo "cli-contract-live: daemon not reachable at $DAEMON (STRICT)"
    exit 1
  fi
  echo "cli-contract-live: skipped — daemon not reachable at $DAEMON"
  exit 0
fi

export SOVEREIGN_NO_STALE_WARN=1 SOVEREIGN_QUIET_DEPRECATIONS=1

# ── read-only allowlist (refuse anything else) ───────────────────────────
is_read_only() {
  case "$1" in
    doctor | doctor\ *) return 0 ;;
    "mesh status" | "mesh balance") return 0 ;;
    "corpus list" | "corpus status") return 0 ;;
    "chat list" | "chat list "* | "chat inspect" | "chat inspect "* | "chat show" | "chat show "*) return 0 ;;
    "mcp list" | "mcp tools" | "mcp tools "*) return 0 ;;
    "tools list" | "tools describe" | "tools describe "*) return 0 ;;
    "atlas status" | "enrich status" | "pipeline status" | "pipeline list") return 0 ;;
    *) return 1 ;;
  esac
}

pass=0
fail=0
refused=0

# Smoke probes come from the manifest as TSV: expect_exit \t args \t substr.
while IFS=$'\t' read -r expect_exit args expect_substr; do
  [ -z "${args:-}" ] && continue
  if ! is_read_only "$args"; then
    echo "REFUSED (not in read-only allowlist): sovereign $args"
    refused=$((refused + 1))
    continue
  fi
  set +e
  # shellcheck disable=SC2086
  out="$("$BIN" $args 2>&1)"
  code=$?
  set -e
  ok=1
  [ "$code" != "${expect_exit:-0}" ] && ok=0
  if [ -n "${expect_substr:-}" ] && ! printf '%s' "$out" | grep -qF "$expect_substr"; then
    ok=0
  fi
  if [ "$ok" = "1" ]; then
    echo "ok:   sovereign $args (exit $code)"
    pass=$((pass + 1))
  else
    echo "FAIL: sovereign $args (exit $code, want ${expect_exit:-0}${expect_substr:+, substr '$expect_substr'})"
    fail=$((fail + 1))
  fi
done < <("$BIN" __contract-smoke)

echo
echo "cli-contract-live: $pass passed, $fail failed, $refused refused"
{ [ "$fail" = "0" ] && [ "$refused" = "0" ]; } && exit 0 || exit 1
