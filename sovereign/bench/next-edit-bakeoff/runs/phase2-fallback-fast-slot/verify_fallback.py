#!/usr/bin/env python3
"""End-to-end verification of the next-edit graceful-degradation fallback.

Run against a live daemon. Prints one PASS/FAIL line per claim so a
failure names which claim broke, not just "something is wrong".

Usage:  verify_fallback.py <expect-fallback: on|off>
"""
import json
import sys
import time
import urllib.error
import urllib.request

BASE = "http://localhost:9741"
results = []


def check(name, ok, detail=""):
    results.append((ok, name, detail))
    print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f"\n          {detail}" if detail else ""))


def get(path, timeout=20):
    with urllib.request.urlopen(f"{BASE}{path}", timeout=timeout) as r:
        return json.load(r)


def post(path, body, timeout=180):
    req = urllib.request.Request(
        f"{BASE}{path}", data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"_raw": raw}


expect_on = sys.argv[1] == "on" if len(sys.argv) > 1 else True
print(f"\n=== next-edit fallback verification (expect fallback {'ARMED' if expect_on else 'OFF'}) ===\n")

status = get("/status")
inf = status.get("inference", {})
edit = inf.get("edit")
fim_mirror = inf.get("fim")

# ── 1. The slot itself ────────────────────────────────────────────
if not expect_on:
    check("flag OFF -> no editing slot is installed (default-off is real)",
          edit is None, f"inference.edit = {json.dumps(edit)}")
else:
    check("fallback installs an editing slot", edit is not None,
          f"inference.edit = {json.dumps(edit)}")
    if edit:
        check("slot is marked degraded (provenance: nobody chose this model)",
              edit.get("degraded") is True, f"degraded={edit.get('degraded')}")
        check("next-edit lane is served",
              edit.get("next_edit_format") == "region_instruct",
              f"next_edit_format={edit.get('next_edit_format')!r}")
        check("advice nudge is present and actionable",
              bool(edit.get("advice")) and "[models.edit]" in (edit.get("advice") or ""),
              (edit.get("advice") or "<none>")[:150])
        check("slot aliases the always-resident fast slot (no extra load)",
              edit.get("aliased_to_fast") is True and edit.get("slot") == "fast",
              f"slot={edit.get('slot')!r} aliased_to_fast={edit.get('aliased_to_fast')}")

# ── 2. The deprecated wire mirror ─────────────────────────────────
check("deprecated inference.fim mirror is byte-identical to inference.edit",
      fim_mirror == edit,
      "mirror differs" if fim_mirror != edit else "identical")

# ── 3. Residency: the fallback must never trigger a model load ────
resident = {s["role"]: s for s in inf.get("resident", [])}
fast = resident.get("fast")
if fast:
    check("fast slot is resident (so an editing keystroke pays no model load)",
          fast.get("resident") is True,
          f"fast={fast.get('model_id')} resident={fast.get('resident')}")
    if expect_on and edit:
        check("the editing slot IS the resident fast slot, not the lazy primary",
              edit.get("model_id") == fast.get("model_id"),
              f"edit.model_id={edit.get('model_id')!r} fast.model_id={fast.get('model_id')!r}")
primary = resident.get("primary")
if primary:
    print(f"          (primary {primary.get('model_id')} resident={primary.get('resident')} "
          f"— idle-unloaded is expected and is why we do not route here)")

# ── 4. FIM lane must refuse rather than guess markers ─────────────
if expect_on and edit is not None and edit.get("fim_style") is None:
    code, body = post("/v1/completions", {
        "model": edit.get("model_id"), "prompt": "def add(a, b):\n    ",
        "suffix": "\n", "max_tokens": 16})
    msg = json.dumps(body)
    check("/v1/completions 503s when the slot has no FIM lane",
          code == 503, f"HTTP {code}")
    check("...and names the real cause, not 'unconfigured'",
          "no FIM markers" in msg,
          msg[:200])
    check("...and says next-edit is unaffected",
          "Next-edit" in msg or "next-edit" in msg, msg[:200])
elif expect_on and edit is not None:
    check("slot reports a FIM lane -> /v1/completions should serve",
          True, f"fim_style={edit.get('fim_style')!r}")

# ── 5. Thinking suppression works on THIS model ───────────────────
if expect_on and edit:
    mid = edit.get("model_id")
    probe = {"model": mid, "max_tokens": 220, "temperature": 0.0,
             "messages": [{"role": "user", "content":
                           "Reply with exactly the word: READY"}]}
    t0 = time.monotonic()
    _, off = post("/v1/chat/completions", dict(probe,
                  chat_template_kwargs={"enable_thinking": False}, think_budget=0))
    t_off = (time.monotonic() - t0) * 1000
    ch = (off.get("choices") or [{}])[0]
    content = (ch.get("message") or {}).get("content") or ""
    finish = ch.get("finish_reason")
    check("thinking suppressed -> non-empty content within a small budget",
          bool(content.strip()) and finish != "length",
          f"finish={finish!r} content={content.strip()[:80]!r} ({t_off:.0f}ms)")

    t0 = time.monotonic()
    _, on = post("/v1/chat/completions", probe)
    t_on = (time.monotonic() - t0) * 1000
    ch2 = (on.get("choices") or [{}])[0]
    c2 = (ch2.get("message") or {}).get("content") or ""
    f2 = ch2.get("finish_reason")
    print(f"\n  [contrast] thinking left at model default: finish={f2!r} "
          f"content={c2.strip()[:60]!r} ({t_on:.0f}ms)")

# ── 6. The route still answers ────────────────────────────────────
cases = [json.loads(l) for l in open("gym/next-edit/gen/cases.jsonl")]
code, body = post("/v1/edit_predictions", cases[0]["request"])
dbg = body.get("sovereign_debug", {})
check("/v1/edit_predictions answers 200 with a glassbox block",
      code == 200 and "model" in dbg, f"HTTP {code} model_state={dbg.get('model_state')!r}")
if expect_on:
    check("model lane no longer reports 'unavailable'",
          dbg.get("model", {}).get("dropped") != "unavailable",
          f"model={json.dumps(dbg.get('model'))[:160]}")

bad = sum(1 for ok, _, _ in results if not ok)
print(f"\n=== {len(results) - bad}/{len(results)} claims passed, {bad} failed ===\n")
sys.exit(1 if bad else 0)
