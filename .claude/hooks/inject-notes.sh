#!/bin/sh
# sovereign inject-notes — UserPromptSubmit hook for Claude Code.
#
# Injects the notes most RELEVANT to the current prompt from the sovereign
# external brain, instead of a flat recency dump.
#
# It also carries the SESSION HANDOFF (MEMORY_MODEL §5 E5 Phase 2): on the
# first prompt of a session it injects the best-matching session frame in
# full. That lives here rather than in session-boot.sh because SessionStart
# has no prompt to select against, and selecting by recency alone hands
# sessions the wrong workstream's frame.
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
import os, re, json, time, hashlib, pathlib, subprocess, urllib.request, urllib.error, socket

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
BASE = f"http://localhost:{PORT}"

# Retrieval log — the encode-time half of the E2/P4 rational-forgetting
# instrument (MEMORY_MODEL §5 E2). Every injection appends ONE record naming
# exactly which notes entered context this turn; the `sovereign notes
# retrieval-audit` command later joins these against the session transcript to
# measure whether an injected note was actually USED downstream. Measure before
# tuning: this log is the baseline the need-probability ranker must beat.
RETRIEVAL_LOG_DIR = pathlib.Path.home() / ".sovereign" / "retrieval-log"

# THE PAYLOAD BUDGET IS LOAD-BEARING (MEMORY_MODEL §5 E5, measured 2026-07-26).
# Claude Code spills hook output over ~10KB to a file and shows the agent only a
# 2KB preview. This hook was printing 8 notes at full length — 15KB payloads
# observed — so ~85% of what it logged as "injected" never reached the model at
# all, while `retrieval-audit` counted every one of them in its denominator.
# Under budget: notes are delivered whole, in rank order, until the budget is
# spent; the remainder is named (not silently dropped) and the log records
# `delivered` per note so the E2 hit-rate is measured against what actually
# entered context.
NOTES_BUDGET_CHARS = int(os.environ.get("SOVEREIGN_NOTES_BUDGET_CHARS", "6000"))
# A note longer than this is truncated rather than allowed to eat the budget
# alone; the id is printed so the agent can dereference the full text.
NOTE_MAX_CHARS = int(os.environ.get("SOVEREIGN_NOTE_MAX_CHARS", "2000"))

# ── First-prompt frame handoff (MEMORY_MODEL §5 E5 Phase 2) ─────────────────
# SessionStart injects only the frame INDEX, because it has no prompt to select
# against and newest-mtime selection demonstrably hands sessions the wrong
# thread's frame. The full frame is injected HERE instead, on the first prompt
# of a session, where the prompt exists and can break ties the boot hook could
# not see.
#
# Once per session: the marker below is written on the first attempt whether or
# not a frame was found, so this never becomes a per-turn subprocess.
#
# The two budgets are shared, not stacked. A frame plus a full notes payload
# would land around 10.5KB and spill to a file — re-creating on turn one the
# exact leak Phase 1 closed. On the frame-bearing turn, notes yield.
SESSIONS_ROOT = pathlib.Path(
    os.environ.get("SVRNMESH_SESSIONS_DIR")
    or os.environ.get("SOVEREIGN_SESSIONS_DIR")
    or (pathlib.Path.home() / ".sovereign" / "sessions")
)
FRAME_BUDGET_CHARS = int(os.environ.get("SOVEREIGN_PROMPT_FRAME_CHARS", "4500"))
NOTES_BUDGET_WITH_FRAME = int(os.environ.get("SOVEREIGN_NOTES_BUDGET_WITH_FRAME", "3200"))


def status(msg):
    # Honest, single-line status — becomes agent context. Prefixed so the agent
    # can tell a hook-level diagnostic from an actual note.
    print(f"_(sovereign brain: {msg})_")


def sha16(s):
    return hashlib.sha256((s or "").encode()).hexdigest()[:16]


def distinctive_terms(content, cap=15):
    # Extract identifier-like tokens the audit can match against the session's
    # downstream actions WITHOUT re-reading the note store (keeps the audit a
    # pure log+transcript join). "Distinctive" = carries a code/path shape
    # (snake_case, CamelCase, dotted, slashed) or is simply long — the classes
    # of token unlikely to co-occur by chance in generic prose. Anchorless notes
    # (empty symbols/files, ~5 of every 8 observed) are only measurable through
    # these terms, so this is what gives the hit-rate coverage.
    out, seen = [], set()
    for t in re.findall(r"[A-Za-z_][A-Za-z0-9_./-]{4,}", content or ""):
        tl = t.lower()
        if tl in seen:
            continue
        distinctive = (
            "_" in t or "." in t or "/" in t
            or any(c.isupper() for c in t[1:])
            or len(t) >= 8
        )
        if not distinctive:
            continue
        seen.add(tl)
        out.append(t)
        if len(out) >= cap:
            break
    return out


def budget_notes(notes, budget):
    """Split `notes` (rank order) into what fits the payload budget and what
    doesn't. Returns (rendered, decisions) where `rendered` is the list of
    (note, text) actually printed and `decisions` is a per-note record of
    delivered/truncated for the retrieval log.

    Greedy fill in rank order: a note too large for the remaining budget is
    skipped and a later, smaller one may still land. That trades strict
    rank-prefix semantics for more notes delivered — acceptable because the
    top-ranked note is guaranteed to land regardless of size, and `delivered`
    records the truth either way."""
    rendered, decisions, spent = [], [], 0
    for n in notes:
        content = (n.get("content") or "").strip()
        truncated = False
        if len(content) > NOTE_MAX_CHARS:
            content = (content[:NOTE_MAX_CHARS].rstrip()
                       + f"\n… [truncated; full note: `sovereign notes --query \"{(n.get('id') or '')[:8]}\"`]")
            truncated = True
        block = f"[{n.get('kind', 'note')}] {content}\n"
        # Always deliver the top-ranked note even if it alone exceeds the
        # budget — an empty injection is worse than one over-long note.
        if spent + len(block) > budget and rendered:
            decisions.append((n, False, False, 0))
            continue
        rendered.append((n, block))
        decisions.append((n, True, truncated, len(block)))
        spent += len(block)
    return rendered, decisions


def log_injection(session_id, query, label, decisions, budget):
    # Append-only, fail-silent side effect. The injection is this hook's primary
    # duty (see the dependability contract above) — logging must NEVER interfere
    # with it, so every failure here is swallowed. No session_id => no join key
    # for the audit, so skip rather than write an unattributable record.
    if not session_id:
        return
    try:
        RETRIEVAL_LOG_DIR.mkdir(parents=True, exist_ok=True)
        record = {
            "ts": int(time.time()),
            "session_id": session_id,
            "prompt_sha": sha16(query),
            "query": (query or "")[:200],
            "label": label,
            "count": len(decisions),
            "delivered_count": sum(1 for _, d, _, _ in decisions if d),
            "budget_chars": budget,
            "payload_chars": sum(c for _, d, _, c in decisions if d),
            "notes": [
                {
                    "rank": i,
                    "id": n.get("id"),
                    "kind": n.get("kind", "note"),
                    "content_sha": sha16((n.get("content") or "").strip()),
                    "symbols": n.get("symbols") or [],
                    "files": n.get("files") or [],
                    "terms": distinctive_terms(n.get("content") or ""),
                    "approx_tokens": len((n.get("content") or "")) // 4,
                    # Did this note actually reach the model's context? Notes
                    # past the payload budget are logged as retrieved-but-not-
                    # delivered so the hit-rate denominator stays honest.
                    "delivered": delivered,
                    "truncated": truncated,
                }
                for i, (n, delivered, truncated, _) in enumerate(decisions)
            ],
        }
        with (RETRIEVAL_LOG_DIR / f"{session_id}.jsonl").open("a", encoding="utf-8") as f:
            f.write(json.dumps(record) + "\n")
    except (OSError, TypeError, ValueError):
        pass


def git(*args):
    try:
        p = subprocess.run(["git", *args], capture_output=True, text=True, timeout=3)
        return p.stdout.strip() if p.returncode == 0 else ""
    except Exception:
        return ""


def inject_frame_once(session_id, query):
    """Inject the best-matching session frame, once per session.

    Returns the number of chars printed (0 if nothing was injected), so the
    caller can shrink the notes budget by that much.

    Every exit path writes the marker: a session that found no frame must not
    re-run this on every subsequent prompt. Every exit path also records WHY,
    because `cache-audit --ramp --classify` can only separate "re-read what
    the frame already had" from "genuine new-task acquisition" if it knows
    which frame — if any — this session actually received.
    """
    if not session_id:
        return 0
    marker = SESSIONS_ROOT / session_id / "frame-inject.json"
    if marker.exists():
        return 0

    rec = {"ts": int(time.time()), "session_id": session_id, "outcome": "none",
           "chosen": None, "candidates": 0, "chars": 0, "signals": {}}
    printed = 0
    try:
        # Boot already injected a frame whole — nothing to add, and
        # re-injecting would duplicate 1-2k tokens. Three shapes qualify, all
        # of them deterministic: `own_full` (resume/compact of this session),
        # `lineage` (this terminal's previous occupant), `attached` (a human
        # pointed this window at a workstream). Only `index` leaves selection
        # to be done here, where a prompt exists to select against.
        boot = SESSIONS_ROOT / session_id / "boot.json"
        if boot.exists():
            try:
                sel = json.loads(boot.read_text()).get("frame_selection")
                if sel in ("own_full", "lineage", "attached"):
                    rec["outcome"] = f"already_injected_at_boot_{sel}"
                    return 0
            except (OSError, ValueError):
                pass

        repo = os.path.basename(git("rev-parse", "--show-toplevel"))
        branch = git("rev-parse", "--abbrev-ref", "HEAD")
        # `--self` so the window pointer boot just wrote (pointing at US) is
        # not mistaken for a predecessor. This path only runs when boot found
        # no lineage, so there is genuinely a selection to make here.
        p = subprocess.run(
            ["sovereign", "session", "frames", "--json", "--repo", repo,
             "--branch", branch, "--for-prompt", (query or "")[:400], "--limit", "3",
             "--self", session_id],
            capture_output=True, text=True, timeout=10,
        )
        if p.returncode != 0 or not p.stdout.strip():
            rec["outcome"] = f"frames_call_failed_rc{p.returncode}"
            return 0
        doc = json.loads(p.stdout)
        cands = doc.get("candidates") or []
        rec["candidates"] = doc.get("count", len(cands))
        if not cands:
            rec["outcome"] = "no_frames"
            return 0
        top = cands[0]
        rec["chosen"] = top.get("session_id")
        rec["signals"] = top.get("signals") or {}
        text = pathlib.Path(top["path"]).read_text()
        truncated = False
        if len(text) > FRAME_BUDGET_CHARS:
            text = (text[:FRAME_BUDGET_CHARS].rstrip()
                    + f"\n\n_[frame truncated — full: `sovereign session frames "
                      f"{top.get('short_id', '')}`]_")
            truncated = True
        others = rec["candidates"] - 1
        header = (
            f"## Session handoff — frame `{top.get('short_id', '')}` "
            f"(selected for this prompt; {others} other live frame"
            f"{'s' if others != 1 else ''})\n"
        )
        footer = (
            "\n_Selected by branch match, then prompt overlap, then recency — "
            "it can be wrong. `sovereign session frames` lists the rest._\n"
            if others > 0 else ""
        )
        block = header + "\n" + text + "\n" + footer
        print(block)
        printed = len(block)
        rec["outcome"] = "injected"
        rec["chars"] = printed
        rec["truncated"] = truncated
        return printed
    except Exception as e:
        rec["outcome"] = f"error_{type(e).__name__}"
        return printed
    finally:
        try:
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text(json.dumps(rec))
        except OSError:
            pass


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
    session_id = (payload.get("session_id") or "").strip()

    # 0) The session handoff, once, before anything that can fail on the
    #    daemon. Reading a frame is filesystem work — a dead daemon must not
    #    cost the successor its predecessor's state.
    frame_chars = inject_frame_once(session_id, query)
    notes_budget = NOTES_BUDGET_WITH_FRAME if frame_chars else NOTES_BUDGET_CHARS

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

    # Budget BEFORE logging: the log must describe what actually entered
    # context, not what the ranker returned. (Pre-2026-07-26 it described the
    # latter, and the payload silently spilled — see NOTES_BUDGET_CHARS.)
    rendered, decisions = budget_notes(uniq, notes_budget)
    log_injection(session_id, query, label, decisions, notes_budget)

    print(f"## Sovereign notes ({label}, injected by hook)\n")
    for _, block in rendered:
        print(block)
    dropped = len(decisions) - len(rendered)
    if dropped:
        print(f"_({dropped} further note{'s' if dropped > 1 else ''} matched but "
              f"exceeded this hook's {notes_budget}-char injection budget — "
              f"they are NOT in your context; `notes(query: \"…\")` to pull them.)_\n")


try:
    main()
except Exception as e:
    # Last-resort guard: never crash silently. Even an unexpected interpreter
    # error yields an honest, actionable line rather than an empty injection.
    print(f"_(sovereign brain: hook error ({type(e).__name__}) "
          "— notes not injected; run .claude/hooks/inject-notes.sh manually to debug)_")
PY
