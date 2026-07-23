#!/bin/sh
# sovereign inject-notes — UserPromptSubmit hook for Claude Code.
#
# Injects the notes most RELEVANT to the current prompt from the sovereign
# external brain, instead of a flat recency dump.
#
# DEPENDABILITY CONTRACT (this hook must never mislead the agent):
#   - It distinguishes failure modes HONESTLY. A genuine daemon outage, a
#     reachable-but-slow brain (cold embed on the first call of a session), an
#     HTTP error, and a changed tool contract each produce a DISTINCT, accurate
#     one-line status — never a blanket "daemon down" that lies when the daemon
#     is actually up. (That false negative is exactly what this rewrite fixes:
#     the old hook mapped every urlopen/JSON failure to "daemon down", so a 3s
#     timeout on a cold embed looked like an outage.)
#   - It PROBES liveness first (GET /status) to separate "daemon is down" from
#     "the notes call failed" — two different truths that demand two different
#     messages and two different operator actions.
#   - Once the daemon is confirmed up, it gives the notes query room for the
#     cold-embed latency, and on timeout falls back to recency-ranked notes and
#     SAYS it did so, rather than injecting nothing.
#
# stdin is the UserPromptSubmit payload (JSON with a `prompt` field). We capture
# it before handing the heredoc to python.

export SOVEREIGN_HOOK_INPUT="$(cat)"
export SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}"

exec python3 - <<'PY' 2>/dev/null
import os, json, hashlib, urllib.request, urllib.error, socket

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
BASE = f"http://localhost:{PORT}"


def status(msg):
    # Honest, single-line status — becomes agent context. Prefixed so the agent
    # can tell a hook-level diagnostic from an actual note.
    print(f"_(sovereign brain: {msg})_")


def is_timeout(e):
    # A read/connect timeout can surface as socket.timeout (== TimeoutError on
    # 3.10+) directly, or wrapped inside URLError.reason. Detect both.
    if isinstance(e, (socket.timeout, TimeoutError)):
        return True
    reason = getattr(e, "reason", None)
    return isinstance(reason, (socket.timeout, TimeoutError))


def describe_failure(e):
    # One honest phrase per failure mode — the daemon is already confirmed up,
    # so these all describe a CALL/CONTRACT problem, never an outage.
    if is_timeout(e):
        return "the notes query timed out"
    if isinstance(e, urllib.error.HTTPError):
        return f"the notes call returned HTTP {e.code}"
    if isinstance(e, (KeyError, IndexError, ValueError, TypeError)):
        return "the notes tool returned an unexpected shape (tool contract changed?)"
    return f"the notes call failed ({type(e).__name__})"


def mcp_notes(args, timeout):
    body = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        # `notes` is the current tool name (the daemon still accepts the old
        # `read_notes` alias, but new code uses the short name).
        "params": {"name": "notes", "arguments": args},
    }).encode()
    req = urllib.request.Request(
        f"{BASE}/mcp", data=body, headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        outer = json.loads(r.read().decode())
    # A shape change here (KeyError/IndexError/ValueError) is a CONTRACT problem,
    # not an outage — the caller categorizes it as such.
    return json.loads(outer["result"]["content"][0]["text"]).get("notes", [])


def main():
    try:
        payload = json.loads(os.environ.get("SOVEREIGN_HOOK_INPUT") or "{}")
    except Exception:
        payload = {}
    query = (payload.get("prompt") or "").strip()[:400]

    # 1) Liveness probe — separates "daemon is genuinely down" from "the notes
    #    call itself failed". These are different truths.
    try:
        urllib.request.urlopen(f"{BASE}/status", timeout=2).read(1)
    except Exception as e:
        if is_timeout(e):
            status(f"daemon reachable on :{PORT} but /status timed out (busy) "
                   "— notes not injected this turn")
        else:
            reason = getattr(e, "reason", e)
            status(f"daemon not reachable on :{PORT} ({reason}) "
                   "— notes not injected")
        return

    # 2) Daemon is UP. Anything below is a call/contract failure, never an outage.
    base_args = {"kinds": ["invariant", "decision"], "limit": 8}
    label = "active"
    notes = None
    if query:
        try:
            # First call of a session embeds the prompt against the embed slot.
            # A cold slot can either exceed a tight budget OR return an error
            # payload outright (observed right after a daemon restart mid-work).
            notes = mcp_notes({**base_args, "query": query, "semantic": True}, 10)
            label = "most relevant to your prompt"
        except Exception as e_sem:
            # Semantic path unavailable (cold embed: timeout OR error payload).
            # The daemon is confirmed up and the plain recency call does NOT
            # touch the embed slot — so fall back before giving up. An agent
            # mid-work should still get its brain through a restart window; we
            # name WHY we fell back so the degradation is visible, not silent.
            try:
                notes = mcp_notes(base_args, 6)
                label = f"recency-ranked (semantic path unavailable: {describe_failure(e_sem)})"
            except Exception as e_rec:
                status(f"up, but {describe_failure(e_rec)} — notes not injected this turn")
                return
    else:
        try:
            notes = mcp_notes(base_args, 6)
        except Exception as e:
            status(f"up, but {describe_failure(e)} — notes not injected this turn")
            return

    # 3) Dedup by content (the store can return the same global note twice).
    seen, uniq = set(), []
    for n in notes or []:
        c = (n.get("content") or "").strip()
        if not c:
            continue
        h = hashlib.sha256(c.encode()).hexdigest()
        if h in seen:
            continue
        seen.add(h)
        uniq.append(n)

    if not uniq:
        return  # daemon healthy, simply nothing relevant — stay silent

    print(f"## Sovereign notes ({label}, injected by hook)\n")
    for n in uniq:
        print(f"[{n.get('kind', 'note')}] {n.get('content', '').strip()}\n")


try:
    main()
except Exception as e:
    # Last-resort guard: never crash silently. Even an unexpected interpreter
    # error yields an honest, actionable line rather than an empty injection.
    print(f"_(sovereign brain: hook error ({type(e).__name__}) "
          "— notes not injected; run .claude/hooks/inject-notes.sh manually to debug)_")
PY
