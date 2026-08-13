#!/bin/sh
# sovereign session-boot — SessionStart hook for Claude Code.
#
# The zero-friction boot: instead of asking every agent to run a session-start
# checklist (recent_changes, project_context, notes, drift_posture, …) and
# read two architecture docs, inject one budgeted artifact at session start:
#
#   Tier 0 — brain health: is the daemon up, how many MCP tools are live.
#   Tier 2 — the session HANDOFF, in three fallbacks, best first:
#            (a) own frame, whole — a resume/compact of this same session_id.
#            (b) PREDECESSOR frame, whole — the session that previously
#                occupied this terminal, looked up by harness-process identity
#                (`sovereign session lineage`). `/clear` mints a new session_id
#                but does NOT restart the harness process, so this is a fact,
#                not a guess, and it is the case that covers ~90% of boots on a
#                working machine (20 of 22 recorded here were `source: clear`).
#            (c) the INDEX — one line per live frame — when there is no
#                lineage at all (a genuinely new terminal). Only here does
#                anything have to be selected, and selection happens later, on
#                the first prompt, where a prompt exists to select against.
#            See docs/specs/SESSION_CONTINUITY.md §3b and MEMORY_MODEL §5 E5.
#   Tier 1 — the working-set brief (`sovereign code brief`): recent activity,
#            relevant notes, drift posture — token-budgeted.
#
# THE PAYLOAD BUDGET IS LOAD-BEARING (MEMORY_MODEL §5 E5, measured 2026-07-26).
# Claude Code spills any hook output over ~10KB to a file and shows the agent
# only a 2KB preview. Sessions then open with `Read <tool-results/hook-*.txt>`
# to get the rest — so an over-budget brief converts itself from budgeted
# context into an UNBUDGETED raw file read, landing in exactly the ramp bucket
# this hook exists to shrink (observed: 40ab6490, 86060bbd, both spilled at
# 11.4KB). Every tier below is capped, and overflow degrades to a pointer the
# agent can dereference on demand (P1) rather than a silent truncation.
#
# DEPENDABILITY CONTRACT (same discipline as inject-notes.sh): every failure
# mode degrades to a distinct, honest one-line status — never a silent skip,
# never a lie. Opt out with SOVEREIGN_NO_BOOT_BRIEF=1.
#
# stdin is the SessionStart envelope (JSON with `session_id`). We capture it
# before handing the heredoc to python, and record what we injected to
# ~/.svrnmesh/sessions/<session_id>/boot.json — the provenance that lets
# `cache-audit --ramp --classify` tell "re-read what the frame already had"
# from "genuine new-task acquisition". Without it, no honest classifier exists.

[ -n "$SOVEREIGN_NO_BOOT_BRIEF" ] && exit 0

export SOVEREIGN_HOOK_INPUT="$(cat)"
export SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}"
export SOVEREIGN_NO_STALE_WARN=1

exec python3 - <<'PY' 2>/dev/null
import json
import os
import re
import subprocess
import time
import urllib.request

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
BASE = f"http://localhost:{PORT}"

# Harness spill threshold measured at ~10KB (smallest observed spill 9.8KB
# across 80 transcripts). Stay well under it: the cost of being 2KB short is
# one dereference; the cost of being 1 byte over is the whole payload turning
# into a raw file read.
#
# Budgets are counted in BYTES, because bytes are what the harness measures.
# This payload is full of `·`, `—` and `✓` at 2-3 bytes each, so a char count
# understates it by ~1.5% (measured: 5578 chars = 5666 bytes) — small now,
# but it is the wrong unit and it always errs toward spilling.
TOTAL_BUDGET_BYTES = int(os.environ.get("SOVEREIGN_BOOT_BUDGET_BYTES", "8000"))
FRAME_BUDGET_BYTES = int(os.environ.get("SOVEREIGN_BOOT_FRAME_BYTES", "4500"))
BRIEF_MIN_BYTES = 800
FRAME_MAX_AGE_DAYS = 14


def nbytes(s):
    return len(s.encode("utf-8"))


def fit_bytes(text, budget, note):
    """Trim `text` so that text+note fits `budget` BYTES. Returns
    (text, truncated). Never silently drops: the caller emits `note`, which
    always names how to fetch the rest."""
    if nbytes(text) <= budget:
        return text, False
    room = max(0, budget - nbytes(note))
    # Cut by characters (never mid-codepoint), then shrink until the encoded
    # length fits. Converges in a couple of passes on real payloads.
    cut = text[:room]
    while cut and nbytes(cut) > room:
        cut = cut[: int(len(cut) * 0.97) or 0]
    return cut.rstrip() + note, True

# Same override the CLI honours (SVRNMESH_/SOVEREIGN_SESSIONS_DIR), so the
# hook and `sovereign session frames` can never read different stores.
SESSIONS_ROOT = (os.environ.get("SVRNMESH_SESSIONS_DIR")
                 or os.environ.get("SOVEREIGN_SESSIONS_DIR")
                 or os.path.expanduser("~/.svrnmesh/sessions"))

try:
    envelope = json.loads(os.environ.get("SOVEREIGN_HOOK_INPUT") or "{}")
except Exception:
    envelope = {}
session_id = (envelope.get("session_id") or "").strip()

# What we injected, for boot.json. Every field is observable fact, not intent.
prov = {
    "ts": int(time.time()),
    "session_id": session_id,
    # startup | resume | clear | compact. On resume/compact the session's OWN
    # frame is the correct handoff and newest-mtime picks it naturally; on
    # startup it cannot exist yet. `frame_is_own` disambiguates the two cases
    # so the mis-injection rate isn't polluted by legitimate self-resumes.
    "source": envelope.get("source") or "",
    "budget_bytes": TOTAL_BUDGET_BYTES,
    "frame_is_own": False,
    "frame_session": None,
    "frame_age_s": None,
    "frame_provenance": None,
    "frame_bytes_full": 0,
    "frame_bytes_injected": 0,
    "frame_truncated": False,
    "frame_candidates": 0,
    # own_full | lineage | attached | index — which Tier-2 shape this boot
    # injected. Phase 1 recorded "newest_mtime"; that selector is gone.
    # `lineage`/`attached` are the deterministic paths (Phase 3): no candidate
    # was ranked, so no ranking can have been wrong.
    "frame_selection": "none",
    # Window lineage provenance — what the boot knew about its own terminal.
    # Recorded even on the index path, because "no window" and "a window with
    # no predecessor" are different failures with different repairs.
    "window_key": None,
    "window_pid": None,
    "predecessor": None,
    "predecessor_kind": None,
    "predecessor_has_frame": None,
    "repo": "",
    "branch": "",
    "brief_bytes": 0,
    "brief_truncated": False,
    "payload_bytes": 0,
}

out = []


def emit(text):
    out.append(text)


emit("## Sovereign session boot (injected by session-boot.sh)\n")

# ── Tier 0: brain health ────────────────────────────────────────────────
try:
    urllib.request.urlopen(f"{BASE}/status", timeout=2).read(1)
    body = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
    }).encode()
    req = urllib.request.Request(
        f"{BASE}/mcp", data=body, headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=4) as r:
            tools = json.loads(r.read().decode()).get("result", {}).get("tools", [])
        emit(f"_brain: daemon up · {len(tools)} MCP tools live "
             f"(symbols/callers/facts/code_search/notes are cheaper and exact — "
             f"prefer them over raw Read/grep)_\n")
    except Exception as e:
        emit(f"_brain: daemon up but MCP tools/list failed ({type(e).__name__}) — "
             f"CLI fallback: `sovereign tools call <id>`_\n")
except Exception:
    emit(f"_brain: daemon not reachable on :{PORT} — code intel is dark; "
         f"start it: `sovereign daemon start`; `sovereign doctor` diagnoses_\n")

# ── Tier 0b: build posture — which side of the toolbox am I on? ─────────
#
# On the Halo, native builds only work inside the `sovereign-vulkan` toolbox
# (llama-cpp-sys-4's build script finds no clang on the Fedora host and dies on
# stdbool.h). CLAUDE.md therefore prefixes the gate commands with
# `toolbox run -c sovereign-vulkan` — which is correct from the host and WRONG
# from inside, where nested toolbox calls fail with a bare
# "flatpak-spawn(1) not found" that reads like a broken install.
#
# Agents were rediscovering this by hand every session (`podman ps` → not
# found; `toolbox list` → cryptic failure). It is not a judgement call: podman
# writes /run/.containerenv with the container's name. So state it, once, as
# fact. Costs ~150B of the brief's budget and removes a recurring several-
# thousand-token detour. Machines with no toolbox (the M2 Max lane) say nothing.
try:
    if os.path.isfile("/run/.containerenv"):
        with open("/run/.containerenv") as f:
            env = f.read()
        m = re.search(r'^name="([^"]*)"', env, re.M)
        cname = m.group(1) if m else ""
        if cname:
            emit(f"_build posture: you are INSIDE the `{cname}` toolbox — run "
                 f"`./scripts/sovereign-lint.sh` / `sovereign daemon …` / cargo "
                 f"DIRECTLY. Do NOT prefix with `toolbox run -c {cname}`; nested "
                 f"toolbox calls fail here (`flatpak-spawn(1) not found`)._\n")
        else:
            emit("_build posture: inside an unnamed container — native builds "
                 "unverified here._\n")
    elif os.path.isfile("/usr/bin/toolbox"):
        emit("_build posture: you are on the HOST. Native builds FAIL here "
             "(llama-cpp-sys-4 finds no clang, dies on stdbool.h) — prefix "
             "builds, tests and daemon control with "
             "`toolbox run -c sovereign-vulkan …`._\n")
except OSError:
    pass

# ── Tier 0c: node identity — WHICH MACHINE AM I? ────────────────────────
#
# Everything cross-machine an agent reads — note authors, work-atlas
# claims, peer observations — is stamped with a node id. Until this line
# existed, a session had no idea which of those ids was its own, so a
# peer's "heavy GPU load, holding the slot" claim read exactly like a
# local one and agents routed around constraints that were never theirs
# (reported by the operator, 2026-08-07).
#
# `sovereign mesh status` has the answer, but it's a subprocess against a
# possibly-busy daemon and no agent thinks to run it unprompted. mesh.json
# is the same data, already on disk, and free.
#
# COUPLING, stated: this reads three fields of the format written by
# `sovereign_mesh::persist` — `name`, `self_node_id`, `members[].node_id`
# and `.name`. Node ids serialize as 16-byte JSON arrays; the short form
# agents see elsewhere is `node-` + the first 8 bytes as hex, which is
# what's rendered here so the two are greppably identical.
try:
    mesh_path = os.path.expanduser("~/.sovereign/mesh.json")
    if os.path.isfile(mesh_path):
        with open(mesh_path) as f:
            mesh = json.load(f)
        short = lambda b: "node-" + bytes(b[:8]).hex()
        me = mesh.get("self_node_id")
        if me:
            me_short = short(me)
            names = {
                short(m["node_id"]): m.get("name", "?")
                for m in mesh.get("members", [])
                if m.get("node_id")
            }
            my_name = names.get(me_short, "<unnamed>")
            peers = sorted(n for i, n in names.items() if i != me_short)
            peer_txt = ", ".join(peers) if peers else "none online"
            emit(
                f"_node: you are **{my_name}** (`{me_short}`) on mesh "
                f"`{mesh.get('name', '?')}` · peers: {peer_txt}. Notes and "
                f"work-atlas claims naming a PEER describe that machine, not "
                f"this one — a peer's GPU load or held lock is not your "
                f"constraint. Notes about the CODE apply everywhere._\n"
            )
except (OSError, ValueError, KeyError, TypeError):
    # Absent, unreadable or reshaped mesh.json ⇒ say nothing. A wrong
    # identity is worse than none: it would tell an agent that a peer's
    # machine-state note is its own.
    pass

# ── Tier 2: the session handoff ─────────────────────────────────────────
#
# PHASE 2 (2026-07-26) stopped injecting the newest frame here, because under
# concurrent workstreams "newest" is the successor's frame only by luck, and a
# wrong frame costs more than none (session 40ab6490 burned 5,872 ramp tokens
# hunting for the right one). SessionStart has no prompt to select against, so
# it stopped selecting and injected only the index; full-frame injection moved
# to the first UserPromptSubmit.
#
# PHASE 3 (2026-07-27) removes the guess for the common case instead of moving
# it. `/clear` mints a new session_id, so the successor is never a "resume" and
# the own-frame path never fires — 20 of the 22 boot records on this machine
# are `source: clear`, every one `frame_is_own: false`. Each of those had to
# pick its predecessor out of 25-42 candidates that all matched the branch, so
# the decision fell to prompt-overlap noise ("everything", "continue") and
# recency, and with two terminals open recency is a coin flip. Measured cost,
# 4 minutes before this was written: session 963fc519 — the `/clear` successor
# of a05e2bd1 in the same terminal — was handed the unrelated F9-scheduler
# frame, noticed ("wrong arc"), and hand-fetched its real predecessor.
#
# But `/clear` does not restart the harness process, so the predecessor is a
# LOOKUP: whoever last occupied this terminal. `frames --claim-window <id>`
# performs that exchange — return the previous occupant, record us as the new
# one — and only when it comes back empty do we fall through to the index.


def git(*args):
    try:
        p = subprocess.run(["git", *args], capture_output=True, text=True, timeout=3)
        return p.stdout.strip() if p.returncode == 0 else ""
    except Exception:
        return ""


repo = os.path.basename(git("rev-parse", "--show-toplevel"))
branch = git("rev-parse", "--abbrev-ref", "HEAD")
prov["repo"] = repo
prov["branch"] = branch

# Own frame on resume/compact — inject it whole, it is definitionally correct.
own = os.path.join(SESSIONS_ROOT, session_id, "frame.md") if session_id else ""
if own and os.path.isfile(own):
    try:
        with open(own) as f:
            frame = f.read()
        prov["frame_selection"] = "own_full"
        prov["frame_session"] = session_id
        prov["frame_is_own"] = True
        prov["frame_age_s"] = int(time.time() - os.path.getmtime(own))
        prov["frame_bytes_full"] = nbytes(frame)
        m = re.search(r"^provenance:\s*(\S+)", frame, re.M)
        prov["frame_provenance"] = m.group(1) if m else "unknown"
        frame, prov["frame_truncated"] = fit_bytes(
            frame, FRAME_BUDGET_BYTES,
            f"\n\n_[frame truncated at {FRAME_BUDGET_BYTES}B — "
            f"read the rest on demand: `Read {own}`]_")
        prov["frame_bytes_injected"] = nbytes(frame)
        emit("### Your own session frame (resumed — this is the state you banked)\n")
        emit(frame)
        emit("")
    except Exception as e:
        emit(f"_own frame at {own} unreadable ({type(e).__name__})_\n")
else:
    prov["frame_selection"] = "index"
    try:
        # The exchange: hand back this terminal's previous occupant, then
        # record us as the occupant so OUR successor is a lookup too. Claiming
        # is safe on every path — a session that turns out to need no handoff
        # still needs to be findable by the session that follows it.
        argv = ["sovereign", "session", "frames", "--json",
                "--repo", repo, "--branch", branch,
                "--limit", "8", "--max-age-days", str(FRAME_MAX_AGE_DAYS)]
        if session_id:
            argv += ["--claim-window", session_id]
        p = subprocess.run(argv, capture_output=True, text=True, timeout=10)
        doc = json.loads(p.stdout) if p.returncode == 0 and p.stdout.strip() else {}
        cands = doc.get("candidates") or []
        prov["frame_candidates"] = doc.get("count", len(cands))
        win = doc.get("window") or {}
        prov["window_key"] = win.get("key")
        prov["window_pid"] = win.get("pid")
        pred = doc.get("predecessor") or {}
        prov["predecessor"] = pred.get("session_id")
        prov["predecessor_kind"] = pred.get("kind")
        prov["predecessor_has_frame"] = pred.get("has_frame")

        # (b) The deterministic handoff. Injected whole, exactly like a resume,
        # because it is the same kind of answer: nothing was selected.
        if pred.get("has_frame") and pred.get("path"):
            frame = open(pred["path"]).read()
            prov["frame_selection"] = (
                "attached" if pred.get("kind") == "explicit" else "lineage"
            )
            prov["frame_session"] = pred.get("session_id")
            prov["frame_age_s"] = pred.get("frame_age_s")
            m = re.search(r"^provenance:\s*(\S+)", frame, re.M)
            prov["frame_provenance"] = m.group(1) if m else "unknown"
            prov["frame_bytes_full"] = nbytes(frame)
            frame, prov["frame_truncated"] = fit_bytes(
                frame, FRAME_BUDGET_BYTES,
                f"\n\n_[frame truncated — full: `sovereign session frames "
                f"{pred.get('short_id', '')}`]_")
            prov["frame_bytes_injected"] = nbytes(frame)
            how = ("you attached this window to it"
                   if pred.get("kind") == "explicit"
                   else "the session that was running in this terminal before "
                        "the last /clear")
            emit(f"### Session handoff — frame `{pred.get('short_id', '')}` "
                 f"({how})\n")
            emit(frame)
            emit("\n_This is not a guess: it is the frame banked by this "
                 "terminal's previous session. If it is the wrong workstream, "
                 "`sovereign session frames` lists the others and "
                 "`sovereign session attach <id>` re-points this window._\n")

            # What is being INHERITED, not just what was done. Pre-rendered by
            # the CLI so this surface and the `session_state` write response
            # can never disagree; absent (and silent) for a healthy handoff.
            # SESSION_CONTINUITY.md §2.2.
            if pred.get("inherited_advice"):
                prov["inherited_carried"] = pred.get("carried_items")
                prov["inherited_worst_frames"] = pred.get("carried_worst_frames")
                prov["objective_sessions"] = pred.get("objective_sessions")
                emit(f"\n{pred['inherited_advice']}\n")

            # A stale terminal binding beside a live frame for the same repo:
            # the lineage answer is still injected (it is an observation), but
            # the fresher frame is NAMED rather than silently outranked. The
            # 2026-08-13 handoff injected a 16h frame as "the" predecessor
            # while an 11-minute in-flight frame existed, and cost the
            # successor 120k+ tokens. Pre-rendered by the CLI so this surface
            # and `sovereign session frames` cannot disagree.
            if pred.get("fresher_advisory"):
                prov["fresher_frame_named"] = True
                emit(f"\n{pred['fresher_advisory']}\n")

        # (b′) A predecessor with no frame is worth saying out loud — the
        # successor should know its lineage resolved but the donor banked
        # nothing, rather than silently reading it as "fresh start".
        elif pred.get("session_id"):
            emit(f"_This terminal's previous session "
                 f"(`{pred.get('short_id', '')}`) banked no frame — nothing to "
                 f"hand off. Index below._\n")

        # (c) No lineage: fall back to the index, as before.
        if prov["frame_selection"] == "index" and cands:
            # Rendered here rather than shelling a second time for the human
            # view; `sovereign session frames` is the authoritative renderer
            # and prints the same facts.
            lines = [f"### Live session frames ({prov['frame_candidates']}) — "
                     f"read one in full: `sovereign session frames <id>`\n"]
            for c in cands:
                sig = c.get("signals") or {}
                marks = []
                if sig.get("branch_match"):
                    marks.append("this branch")
                if sig.get("in_flight"):
                    marks.append("in-flight")
                mark = f" · {' · '.join(marks)}" if marks else ""
                age = c.get("age_s") or 0
                age_s = (f"{age // 60}m" if age < 3600
                         else f"{age // 3600}h" if age < 86400
                         else f"{age // 86400}d")
                lines.append(
                    f"- `{c.get('short_id', '')}` · {age_s}{mark} · "
                    f"{c.get('next_items', 0)} next — {c.get('goal', '')}"
                )
            why = ("this terminal has no recorded predecessor (first session "
                   "in a new window)" if win else
                   "no harness window could be resolved, so lineage is "
                   "unavailable here")
            lines.append(f"\n_No frame is injected: {why}. Pick the one that "
                         "describes work you are continuing — and if you know "
                         "which it is, `sovereign session attach <id>` makes "
                         "the next /clear in this window deterministic._\n")
            block = "\n".join(lines)
            prov["frame_bytes_injected"] = nbytes(block)
            emit(block)
        # No fresh frame is normal (first boot, or >14d idle) — say nothing.
    except FileNotFoundError:
        emit("_frame index unavailable (`sovereign` not on PATH)_\n")
    except Exception as e:
        emit(f"_frame index unavailable ({type(e).__name__})_\n")

# ── Tier 2c: open work orders (comaintainer artifact 4) ────────────────
# One line per OPEN order under .sovereign/features/*/order.md — the
# worker-facing half of the order loop (docs/COMAINTAINER.md §10.4).
# Opt-in by construction: no orders → not a single line; opt out even
# of the index with SOVEREIGN_NO_ORDERS=1. A session without an order
# behaves exactly as before this tier existed.
if not os.environ.get("SOVEREIGN_NO_ORDERS"):
    try:
        import glob as _glob
        _repo = os.environ.get("CLAUDE_PROJECT_DIR") or os.getcwd()
        _orders = []
        for _p in sorted(_glob.glob(os.path.join(
                _repo, ".sovereign", "features", "*", "order.md"))):
            try:
                _head = open(_p, encoding="utf-8", errors="replace").read(2048)
            except OSError:
                continue
            _st = re.search(r"^status:\s*(\S+)", _head, re.M)
            if not _st or _st.group(1) != "open":
                continue
            _ti = re.search(r"^# Order:\s*(.+)$", _head, re.M)
            _orders.append((os.path.basename(os.path.dirname(_p)),
                            (_ti.group(1).strip() if _ti else "")[:80]))
        if _orders:
            _lines = [f"### Open work orders ({len(_orders)})\n"]
            for _oid, _title in _orders[:8]:
                _lines.append(f"- `{_oid}` — {_title}  "
                              f"(`.sovereign/features/{_oid}/order.md`)")
            if len(_orders) > 8:
                _lines.append(f"- …and {len(_orders) - 8} more")
            _lines.append("\n_If this session is picking one up, Read it "
                          "whole first — it carries objective, scope to "
                          "claim, lane, budget, seams. If not, ignore this "
                          "block; orders are opt-in._\n")
            emit("\n".join(_lines))
    except Exception as e:
        emit(f"_order index unavailable ({type(e).__name__})_\n")

# ── Tier 1: working-set brief ───────────────────────────────────────────
spent = sum(nbytes(p) + 1 for p in out)
brief_budget = max(BRIEF_MIN_BYTES, TOTAL_BUDGET_BYTES - spent)
try:
    proc = subprocess.run(
        ["sovereign", "code", "brief", "--strategy", "recent", "--hours", "48",
         "--budget", "1200"],
        capture_output=True, text=True, timeout=15,
    )
    if proc.returncode == 0 and proc.stdout.strip():
        brief, prov["brief_truncated"] = fit_bytes(
            proc.stdout.strip(), brief_budget,
            "\n\n_[brief truncated to stay under the hook payload budget — "
            "full: `sovereign code brief --hours 48`]_")
        prov["brief_bytes"] = nbytes(brief)
        emit(brief)
    else:
        err = (proc.stderr or proc.stdout).strip().splitlines()
        emit(f"_working-set brief unavailable (sovereign code brief exit "
             f"{proc.returncode}: {err[-1][:120] if err else 'no output'})_")
except FileNotFoundError:
    emit("_working-set brief unavailable (`sovereign` not on PATH — "
         "ln -sf $(realpath sovereign/target/debug/sovereign-cli) ~/.local/bin/sovereign)_")
except subprocess.TimeoutExpired:
    emit("_working-set brief unavailable (sovereign code brief timed out at 15s)_")

payload = "\n".join(out)
# Bytes — the unit the harness spill threshold is actually in.
prov["payload_bytes"] = nbytes(payload)
print(payload)

# ── Provenance sidecar (fail-silent: never break the boot) ──────────────
if session_id:
    try:
        d = os.path.join(SESSIONS_ROOT, session_id)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "boot.json"), "w", encoding="utf-8") as f:
            json.dump(prov, f)
    except OSError:
        pass
PY
