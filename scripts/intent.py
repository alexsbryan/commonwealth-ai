#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""
INTENT — the argument that produced a symbol, and whether its evidence holds.

The end-to-end slice of the intent model (order: .sovereign/features/intent-model):
for a symbol S, return the argument that produced S — claim, objection,
concession — and a verdict on whether the evidence that argument cites
actually sees the change. Breadth first: every hop is thin and every hop is
an existing seam, so the run tells us where to deepen.

  symbol ──symbols──▶ file:range ──git log -L──▶ commits
         ──body──▶ argument (local daemon; deterministic fallback, NAMED)
         ──evidence-verdict records / static class──▶ verdict
         ──note tagged with the symbol──▶ shown by .claude/hooks/intent-warn.py
                                          at the moment of the edit

  scripts/intent.py open_index_transient            # print the record
  scripts/intent.py open_index_transient --note     # and write it to the notes store
"""
import argparse, importlib.util, json, re, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERDICTS = ROOT / ".sovereign/features/intent-model/data/move1-verdicts.jsonl"

_ev_spec = importlib.util.spec_from_file_location("evidence_verdict", ROOT / "scripts/evidence-verdict.py")
ev = importlib.util.module_from_spec(_ev_spec)
_ev_spec.loader.exec_module(ev)

sys.path.insert(0, str(ROOT / "gym" / "comaintainer"))
try:
    from score import call_daemon  # the ONE schema-forced daemon call (co_liveness.py takes the same import)
except Exception as exc:  # named, never defaulted
    call_daemon, _DAEMON_ERR = None, exc


def sh(*argv, cwd=ROOT) -> str:
    return subprocess.run(argv, cwd=cwd, capture_output=True, text=True).stdout


# ── hop 1: symbol -> file:range ────────────────────────────────────────────
def resolve(symbol: str):
    out = sh("sovereign", "tools", "call", "symbols", f"--name={symbol}", "--format", "json")
    for m in re.finditer(r"//\s*([^\s:]+):(\d+)-(\d+)", out):
        return m.group(1), int(m.group(2)), int(m.group(3))
    return None


# ── hop 2: file:range -> commits ───────────────────────────────────────────
def commits_for(path: str, a: int, b: int, n: int = 3) -> list:
    out = sh("git", "log", f"-L{a},{b}:{path}", "--format=%H", "-s", f"-n{n}")
    return [l for l in out.split() if re.fullmatch(r"[0-9a-f]{40}", l)][:n]


# ── hop 3: commit body -> argument ─────────────────────────────────────────
SCHEMA = {"type": "object", "required": ["claim", "objection", "concession"],
          "properties": {k: {"type": "string"} for k in ("claim", "objection", "concession")}}
PROMPT = """Read this commit message and extract its argument. Quote or closely paraphrase the text; never invent.
claim: what the change asserts it achieves (one sentence).
objection: the alternative or counter-argument the author names and answers (one sentence), or "none".
concession: what was given up, deferred, excluded or left open, and why (one sentence), or "none".
Reply as JSON with keys claim, objection, concession.

COMMIT:
{body}"""
OBJECTION_RE = re.compile(r"\b(instead of|rejected|is not the|would (?:have|not)|cannot|objection|not because|"
                          r"the (?:naive|obvious|first) (?:fix|version|cut)|is no defence)\b", re.I)
CONCESSION_RE = re.compile(r"\b(conced|deferred|not here|priced as|excluded|out of scope|left open|"
                           r"stays (?:off|preview)|not fixed here|follow-?up|a seam|does not cover|untouched)\w*", re.I)


def resident_general_model():
    """The first resident model advertising a `general` hint, from the daemon
    itself — so a retry pins a NAME the mesh advertises, and the record says
    which one answered. Never an alias."""
    try:
        import urllib.request
        with urllib.request.urlopen("http://localhost:9741/v1/models", timeout=3) as r:
            for m in json.load(r).get("data", []):
                if m.get("residency") == "resident" and any(
                        c.get("hint") == "general" for c in m.get("capabilities", [])):
                    return m["id"]
    except Exception:
        return None
    return None


def argument(body: str) -> dict:
    """The daemon path is the product path; the deterministic slicer is the
    fallback and the record NAMES which one answered (ARCH §18.3). The seat's
    engine of record is tried first; when the mesh does not advertise it, one
    retry pins the resident general model, named in `engine`."""
    reasons = []
    if call_daemon is not None:
        resident = resident_general_model()
        for pin in [None] + ([resident] if resident else []):
            try:
                kw = {"pin": pin} if pin else {}
                text, model = call_daemon(PROMPT.format(body=body[:6000]), 120, 400,
                                          schema=SCHEMA, schema_name="argument", **kw)
                parsed = json.loads(text)
                return {**{k: parsed.get(k, "none") for k in ("claim", "objection", "concession")},
                        "engine": model + (" (retry pin; seat engine not advertised)" if pin else "")}
            except Exception as exc:
                reasons.append(f"pin={pin or 'seat'}: {str(exc)[:100]}")
        reason = "daemon refused or failed — " + "; ".join(reasons)
    else:
        reason = f"daemon caller unavailable: {str(_DAEMON_ERR)[:120]}"
    sents = [" ".join(s.split()) for s in ev.SENTENCE_SPLIT.split(body) if s.strip()]
    pick = lambda rx: next((s[:240] for s in sents if rx.search(s)), "none")
    return {"claim": sents[0][:240] if sents else "none", "objection": pick(OBJECTION_RE),
            "concession": pick(CONCESSION_RE), "engine": f"deterministic slicer ({reason})"}


# ── hop 4: commit -> evidence verdict ──────────────────────────────────────
def verdict(commit: str, body: str) -> dict:
    """A build-based record when one exists; otherwise the static class, and
    NEVER-RAN said in so many words — absence is reported, not defaulted."""
    if VERDICTS.is_file():
        for line in VERDICTS.read_text().splitlines():
            rec = json.loads(line)
            if commit.startswith(rec["commit"]):
                return {"verdict": rec["verdict"], "detail": rec.get("detail", ""),
                        "per_citation": rec.get("per_citation", {}), "basis": "build (evidence-verdict.py)"}
    cites = ev.cited_tests(commit, body)
    if not cites:
        return {"verdict": "NEVER-RAN", "detail": "the body names no test", "basis": "static"}
    s = ev.static_overlap(commit, cites)
    return {"verdict": "NEVER-RAN", "detail": f"not yet judged by build; static class: {s['static']}",
            "static": s["static"], "basis": "static (evidence-verdict.py --static)"}


# ── render + store ─────────────────────────────────────────────────────────
def record(symbol: str, n: int) -> dict:
    loc = resolve(symbol)
    if not loc:
        return {"symbol": symbol, "error": "symbols() did not resolve this name — is the index current?"}
    path, a, b = loc
    entries = []
    for c in commits_for(path, a, b, n):
        subject, _, body = sh("git", "show", "-s", "--format=%s%n%b", c).partition("\n")
        arg = argument(subject + "\n\n" + body)
        entries.append({"commit": c[:9], "subject": subject[:100], **arg, **verdict(c, body)})
    return {"symbol": symbol, "file": path, "lines": [a, b], "history": entries}


def render(r: dict) -> str:
    if "error" in r:
        return f"intent: {r['symbol']}: {r['error']}"
    out = [f"INTENT · {r['symbol']} · {r['file']}:{r['lines'][0]}-{r['lines'][1]}"]
    for e in r["history"]:
        out += [f"  {e['commit']}  {e['subject']}",
                f"    claim:      {e['claim']}",
                f"    objection:  {e['objection']}",
                f"    concession: {e['concession']}",
                f"    evidence:   {e['verdict']} — {e['detail'][:200]}",
                f"    basis:      {e['basis']} · argument by {e['engine']}"]
    return "\n".join(out)


def write_note(r: dict, text: str) -> str:
    cmd = ["sovereign", "tools", "call", "note", "--kind=decision", f"--content={text}",
           f"--symbols={json.dumps([r['symbol']])}", f"--files={json.dumps([r['file']])}"]
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True).stdout.strip()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("symbol")
    ap.add_argument("-n", type=int, default=3, help="how many commits of the symbol's history")
    ap.add_argument("--note", action="store_true", help="write the record to the notes store, tagged with the symbol")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    r = record(a.symbol, a.n)
    text = render(r)
    print(json.dumps(r, indent=1) if a.json else text)
    if a.note and "error" not in r:
        print(write_note(r, text), file=sys.stderr)
    return 1 if "error" in r else 0


if __name__ == "__main__":
    sys.exit(main())
