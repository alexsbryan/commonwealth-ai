#!/usr/bin/env bash
# co-boot-block.sh — ONE constant rail read for a fresh seat session (order
# seat-boot-block, replacing the manual boot query loop the spike measured:
# 10-12 round-trips, 14.5k-51.7k tokens of note bodies per seat session —
# research/comaintainer-memory/SMALL_CONTEXT_MEMORY_SPIKE.md, F3/F4).
#
# Assembles the four sections the skill's boot step 1 prescribes, at a fixed
# budget (~12000 chars ≈ 3k tokens), in priority order:
#
#   1. Open orders          — `co-order.sh list`  (order-seat rail)
#   2. Seat todos           — related_to=comaintainer-seat, kind=todo
#   3. Recent seat decisions— related_to=comaintainer-seat, kind=decision/
#                             commitment/attempt
#   4. Directive log        — `co-directive-log.sh --stats` (directive-log rail)
#
# EVERYTHING is a dereferenceable pointer (P1): one line per note — id, kind,
# first line. Bodies are pulled on demand (P5) with `notes(query: "<terms
# from that first line>")` — the ONLY working dereference: there is no
# exact-id route on the daemon MCP surface (`notes(query: "<id>")` returns
# notes that merely mention the id — measured 2026-08-13 — and `svrn notes
# list --id` reads the repo-local store, not the daemon store). This is not a
# lossy compression of the rail; it is the rail's index, rendered once. When
# the budget is exceeded, claim lines degrade to bare `id [kind]` pointers
# and the overflow is NAMED (never silent truncation) — a line says how many
# further notes live at the anchor read.
#
# Once per session: when SESSION_ID is given, this script writes its own
# existence + note record to ~/.svrnmesh/sessions/<id>/boot-block.json (the
# `injected-notes.json` dedupe pattern). The hook that calls it treats that
# file's presence as "the block already fired". A FAILED run writes NO marker,
# so a transient daemon hiccup on the first prompt does not permanently lose
# the block — the next prompt retries.
#
# The record doubles as the E2 retrieval-log source (MEMORY_MODEL §5 E2): the
# hook converts it into one row of ~/.svrnmesh/retrieval-log/<session>.jsonl
# so the retrieval-audit hit-rate is measured against what actually entered
# context, with `delivered` per note. `delivered` here is true when the note
# made the rendered block (a pointer line is delivery; a body is a
# dereference); a note the budget dropped is `delivered: false`.
#
# Fails silently (exit 0, honest one-line statuses) whenever anything goes
# wrong — a hook must never block a prompt, and absence is reported, never
# defaulted (ARCH §18.3).
#
# Usage:
#   scripts/co-boot-block.sh [<session_id>]   # render (optionally record)
#   SOVEREIGN_PORT=9741 scripts/co-boot-block.sh   # explicit daemon port

set -u
cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)" || exit 0

SESSION_ID="${1:-${SOVEREIGN_SESSION_ID:-}}"

# Same env dance as inject-notes.py — all three surfaces must never read
# different stores.
SESSIONS_ROOT="${SVRNMESH_SESSIONS_DIR:-${SOVEREIGN_SESSIONS_DIR:-$HOME/.svrnmesh/sessions}}"
export SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}"

BLOCK_BUDGET_CHARS="${SOVEREIGN_BOOT_BLOCK_BUDGET_CHARS:-12000}"
# The tool's cap (100) is the DEFAULT on purpose: the seat-anchor graph holds
# ~91 notes and a 40-note cap truncated it — dropping the SEAT STEWARDSHIP
# LOG (measured 2026-08-13). The budget, not the read, decides what renders.
NOTES_LIMIT="${SOVEREIGN_BOOT_BLOCK_LIMIT:-100}"

# ── Sections 1 + 4: the existing co-* scripts are the single implementation ──
# of "list open orders" and "directive-log stats" (ARCH §10.6 — never a second
# implementation of the same read). Their daemon-down fallbacks and honesty
# banners carry into the block unchanged.
ORDERS="$(./scripts/co-order.sh list 2>/dev/null)"
ORDERS_RC=$?
STATS="$(./scripts/co-directive-log.sh --stats 2>/dev/null)"
STATS_RC=$?

export BOOT_BLOCK_ORDERS="$ORDERS"
export BOOT_BLOCK_STATS="$STATS"
export BOOT_BLOCK_ORDERS_RC="$ORDERS_RC"
export BOOT_BLOCK_STATS_RC="$STATS_RC"
export BOOT_BLOCK_SESSION_ID="$SESSION_ID"
export BOOT_BLOCK_BUDGET_CHARS="$BLOCK_BUDGET_CHARS"
export BOOT_BLOCK_NOTES_LIMIT="$NOTES_LIMIT"
export BOOT_BLOCK_SESSIONS_ROOT="$SESSIONS_ROOT"

python3 - <<'PY'
import json, os, re, sys, time, urllib.request

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
SESSIONS_ROOT = os.environ["BOOT_BLOCK_SESSIONS_ROOT"]
SESSION_ID = os.environ.get("BOOT_BLOCK_SESSION_ID") or ""
BUDGET = int(os.environ.get("BOOT_BLOCK_BUDGET_CHARS", "12000"))
LIMIT = int(os.environ.get("BOOT_BLOCK_NOTES_LIMIT", "40"))

ORDERS = os.environ.get("BOOT_BLOCK_ORDERS", "").strip()
STATS = os.environ.get("BOOT_BLOCK_STATS", "").strip()

# ── the notes rail read ──────────────────────────────────────────────────────
# related_to=comaintainer-seat: the T2 entity-graph read (co-occurrence
# ranking) — the deterministic rail the seat sessions actually used. It is
# NOT entity equality: the strict comaintainer-seat entity subset is tiny,
# while the read surfaces ~91 co-occurring records (measured 2026-08-13);
# the query-based path is worse — it mixes in unanchored content-mention
# matches (measured 1/28 anchored). The `kinds` filter is ignored on this
# path (measured 2026-08-13), so the split by kind happens here, client-side:
# todos first, then decisions/commitments/attempts.
def read_rail():
    body = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "notes", "arguments": {
            "related_to": "comaintainer-seat",
            "limit": LIMIT,
            "include_operational": True,
        }},
    }).encode()
    req = urllib.request.Request(
        f"http://localhost:{PORT}/mcp", data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=4) as resp:
        outer = json.load(resp)
    inner = json.loads(outer["result"]["content"][0]["text"])
    return inner.get("notes") or []


def first_line(content, cap=110):
    for line in (content or "").splitlines():
        line = " ".join(line.split())
        if line:
            return line[:cap] + ("…" if len(line) > cap else "")
    return "(empty)"


def distinctive_terms(content, cap=15):
    # Same token-shape heuristic as inject-notes.py's distinctive_terms —
    # KEEP THE TWO IN SYNC (one instrument, two write sites). The E2 audit's
    # content-match signal depends on both producing the same token classes.
    out, seen = [], set()
    for t in re.findall(r"[A-Za-z_][A-Za-z0-9_./-]{4,}", content or ""):
        # a single char repeated (e.g. a filler run) is not a distinctive
        # term — same rule as inject-notes.py's copy (KEEP-THE-TWO-IN-SYNC).
        if len(set(t)) == 1:
            continue
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


def author_tag(note):
    if note.get("author_relation") == "peer":
        return f" _{note.get('author') or 'peer'}_"
    return ""


rail_error = None
try:
    rail = read_rail()
except Exception as e:
    rail = []
    rail_error = f"{type(e).__name__}"

# Todos first, then decisions/commitments/attempts, then EVERYTHING else the
# read returned (follow-ups, invariants, reflections… — 40b5cc0b pulled
# kinds=todo, then decision+commitment+attempt) — no kind is silently dropped.
todos = [n for n in rail if n.get("kind") == "todo"]
core = [n for n in rail if n.get("kind") in ("decision", "commitment", "attempt")]
rest = [n for n in rail if n.get("kind") not in ("todo", "decision", "commitment", "attempt")]

# ── fixed content FIRST (header + tail), then the anchor sections get the
# budget minus everything else — the declared budget must bound the WHOLE
# block, not just the anchor rail.
header_text = (
    "## Seat boot block — the rail, indexed once\n\n"
    "One line per record; when a line is load-bearing, pull its body with "
    "`notes(query: \"<distinctive words from that line>\")` (P5 — dereference "
    "before use; there is no exact-id query — an id alone returns notes that "
    "merely mention it). This block replaces the boot query loop; the manual "
    "ritual is the fallback when it is missing.\n\n"
)

tail = ["### Open orders (order-seat rail)\n\n"]
if ORDERS:
    tail.append(ORDERS + "\n")
else:
    tail.append(f"_order rail unavailable (co-order.sh rc={os.environ.get('BOOT_BLOCK_ORDERS_RC')}) — "
                "`scripts/co-order.sh list` manually_\n")
tail.append("\n### Directive log (directive-log rail, co-directive-log.sh --stats)\n\n")
if STATS:
    # Keep the tally table + the honesty banner lines; drop the explanatory
    # footer (the table self-documents, and the block is an index).
    lines = STATS.splitlines()
    kept = []
    for ln in lines:
        if re.match(r"^(kind|briefing|decision|order|review|steer|ALL)\s", ln):
            kept.append(ln)
        elif ln.startswith("#") or "fallback" in ln.lower() or "not in this denominator" in ln:
            kept.append(ln)
    tail.append("\n".join(kept) if kept else STATS)
else:
    tail.append(f"_directive rail unavailable (co-directive-log.sh rc={os.environ.get('BOOT_BLOCK_STATS_RC')}) — "
                "`scripts/co-directive-log.sh --stats` manually_\n")
tail_text = "".join(tail)

# 300-char reserve for the honesty/status lines the render appends between
# the anchor sections and the tail (dropped-naming + rail-error; measured
# 293 chars on 2026-08-13) — with it, the rendered block is always ≤ BUDGET,
# never "budget plus status".
ANCHOR_BUDGET = max(0, BUDGET - len(header_text) - len(tail_text) - 300)

# ── anchor assembly against ANCHOR_BUDGET ────────────────────────────────────
# Sections render in priority order. Claim lines are the default; when the
# budget is spent, later notes degrade to bare `id [kind]` pointers (still
# dereferenceable), and the overflow is NAMED — never silently dropped.
sections = []
note_records = []          # for the E2 record: {id, kind, symbols, files, delivered, truncated}
budget_spent = 0
dropped = 0

def fits(chars):
    global budget_spent
    if budget_spent + chars <= ANCHOR_BUDGET:
        budget_spent += chars
        return True
    return False

def note_line(nid, kind, n, pointer_only):
    line = f"- `{nid}` [{kind}]{author_tag(n)} {first_line(n.get('content')) if not pointer_only else ''}"
    return line.rstrip() + "\n"

def emit_section(header, notes):
    global dropped
    if not notes:
        return
    head = f"### {header}\n"
    if not fits(len(head)):
        # Whole section skipped: every note still lands in the E2 record as
        # delivered=false, and the overflow naming line below counts them —
        # a section is never silently absent.
        for n in notes:
            dropped += 1
            note_records.append({
                "id": n.get("id"), "kind": n.get("kind", "note"),
                "symbols": n.get("symbols") or [], "files": n.get("files") or [],
                "terms": distinctive_terms(n.get("content") or ""),
                "delivered": False, "truncated": True,
            })
        return
    buf = head
    for n in notes:
        nid = (n.get("id") or "")[:8]
        full = note_line(nid, n.get("kind", "note"), n, pointer_only=False)
        if fits(len(full)):
            buf += full
            note_records.append({
                "id": n.get("id"), "kind": n.get("kind", "note"),
                "symbols": n.get("symbols") or [], "files": n.get("files") or [],
                "terms": distinctive_terms(n.get("content") or ""),
                "delivered": True, "truncated": False,
            })
        else:
            ptr = note_line(nid, n.get("kind", "note"), n, pointer_only=True)
            if fits(len(ptr)):
                buf += ptr
                note_records.append({
                    "id": n.get("id"), "kind": n.get("kind", "note"),
                    "symbols": n.get("symbols") or [], "files": n.get("files") or [],
                    "terms": distinctive_terms(n.get("content") or ""),
                    "delivered": True, "truncated": True,
                })
            else:
                dropped += 1
                note_records.append({
                    "id": n.get("id"), "kind": n.get("kind", "note"),
                    "symbols": n.get("symbols") or [], "files": n.get("files") or [],
                    "terms": distinctive_terms(n.get("content") or ""),
                    "delivered": False, "truncated": True,
                })
    sections.append(buf)

out = [header_text]

# 1. seat todos, 2. recent seat decisions, 3. the rest of the rail — priority
# per the order: "seat-anchor todos first, then recent seat decisions".
emit_section("Seat todos (anchor comaintainer-seat, kind=todo)", todos)
emit_section("Recent seat decisions (anchor comaintainer-seat)", core)
emit_section("Further seat-anchored records (follow-ups, invariants, …)", rest)
out.extend(sections)  # the anchor sections, in priority order
if dropped:
    out.append(f"_… {dropped} further seat note(s) exceed the block's {BUDGET}-char budget — "
               "at `notes(query: \"comaintainer-seat\")`_")
    out.append("")
if rail_error:
    out.append(f"_seat rail unavailable ({rail_error}) — `notes(query: \"comaintainer-seat\")` "
               "manually_")
    out.append("")

# 3. open orders + 4. directive log — the fixed tail, built first and budgeted
# for (the block's declared budget bounds header + anchor sections + tail).
out.append(tail_text)

block = "\n".join(out).rstrip() + "\n"
print(block)

# ── the once-per-session record (marker + E2 source) ─────────────────────────
if SESSION_ID:
    try:
        rec = {
            "ts": int(time.time()),
            "session_id": SESSION_ID,
            "outcome": "injected" if block else "empty",
            "budget_chars": BUDGET,
            "payload_chars": len(block),
            "rail_error": rail_error,
            "notes": note_records,
        }
        d = os.path.join(SESSIONS_ROOT, SESSION_ID)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "boot-block.json"), "w", encoding="utf-8") as fh:
            json.dump(rec, fh)
    except OSError:
        pass  # a hook side-effect must never fail the block
PY

exit 0
