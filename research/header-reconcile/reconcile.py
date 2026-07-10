#!/usr/bin/env python3
"""Header-reconcile spike — protocol + pre-registered gates in README.md.

Phases (checkpointed under out/, resumable):
  claims      — decompose each //! header into class-gated claims
  adjudicate  — per-file: capability claims vs pinned child-fn evidence
  verify      — adversarial second pass on contradictions + silent-growth
  report      — out/report.md (shipped findings + base-rate tables)
  all         — everything

Usage: python3 reconcile.py <phase> [--limit N]
"""

import argparse
import json
import re
import subprocess
import time
from collections import Counter, defaultdict
from pathlib import Path

import requests

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
OUT = HERE / "out"
NODES = HERE.parent / "orientation-bench" / "out" / "nodes.json"
CACHE = Path.home() / ".sovereign/indexes/commonwealth-ai/code_intel_cache.json"
API = "http://localhost:9741/v1"
MODEL = "primary"  # Qwen3.6-35B-A3B-MTP at time of run
EVIDENCE_CHAR_BUDGET = 20000

THINK_RE = re.compile(r"<think>.*?</think>", re.DOTALL)

SYSTEM_DECOMPOSE = """You decompose a Rust file's //! doc header into individual claims.
CRITICAL: every claim statement must be fully self-contained, carrying every qualifier the header attaches to it:
- status qualifiers ("v1 stub", "not yet implemented", "planned", "reserved for future") — if the header says a capability is a stub or inactive, the claim MUST say so
- scope qualifiers ("in this step", "phase 3a only", "when [x] is set")
- exception clauses — a rule and its exception are ONE claim, never two ("zero domain logic except from_recipe" stays together)
A claim stripped of its qualifier is a wrong claim.
Classify each claim:
- capability: what the file does, contains, produces, or guarantees, observable in its functions
- rationale: why a design was chosen, tradeoffs, comparisons to alternatives
- history: when/what changed, past decisions, references to past events
- cross_cutting: constraints about other modules, threads, locks, or system-wide behavior not checkable from this file's functions alone
- reference: pointers to docs, specs, other files
Reply with ONLY this JSON, no other text:
{"claims":[{"statement":"<one claim, self-contained>","claim_class":"capability|rationale|history|cross_cutting|reference"}]}"""

SYSTEM_ADJUDICATE = """You check a Rust file's documented claims against evidence: one-line summaries of every function in the file (with line numbers).
For each numbered claim, verdict:
- corroborated: at least one function summary directly supports it (cite those functions)
- contradicted: a function summary directly conflicts with it (cite it). Only use this when the conflict is explicit, not inferred.
- not_evidenced: the summaries neither support nor conflict. This is a normal outcome; never stretch to corroborated or contradicted.
A claim carrying a stub/inactive/planned qualifier is corroborated when the code matches that stub state (a no-op function corroborates "v1 stub"). Evidence may cover a whole module directory when the header belongs to a mod.rs; a claim about a submodule can be corroborated by any file in it.
Also: if three or more functions together implement a distinct capability that the header text never mentions, name it as silent_growth; otherwise null.
Reply with ONLY this JSON, no other text:
{"verdicts":[{"claim":<n>,"verdict":"corroborated|contradicted|not_evidenced","cite":["fn_name:line"],"note":"<one short sentence>"}],"silent_growth":{"capability":"<short name>","cite":["fn_name:line"]} or null}"""

SYSTEM_VERIFY = """You audit whether a Rust file's //! doc header accurately describes its code. You are given the header, a proposed mismatch finding, and the ACTUAL SOURCE CODE at the cited lines. The source code outranks any summary.
Answer in three steps, in this exact JSON (no other text):
{"code_shows": "<one sentence: what the source at the cited lines actually does>",
 "header_says": "<one sentence: what the header asserts about that>",
 "header_accurate": true|false,
 "reason": "<one sentence citing the decisive source line>"}
header_accurate=false means the header genuinely mismatches the source — even a small factual detail (wrong hash algorithm, wrong signature, wrong owner) counts as inaccurate.
header_accurate=true means the header is fine: the proposed finding stretched evidence, misread the claim, ignored a qualifier the header states (stub / phase scope / exception), or the mismatch is compatible with the header.
If the given source is insufficient to decide, answer header_accurate=true."""

SYSTEM_POLARITY = """You read one sentence written by a code reviewer about whether a doc header matches the code. Classify what the SENTENCE says. Reply with ONLY this JSON:
{"reason_says": "header_wrong" | "header_fine" | "unclear"}"""


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def chat(system: str, user: str, max_tokens: int) -> str:
    last = None
    for attempt in range(3):
        try:
            r = requests.post(
                f"{API}/chat/completions",
                json={
                    "model": MODEL,
                    "max_tokens": max_tokens,
                    "temperature": 0.1,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user},
                    ],
                },
                timeout=600,
            )
            r.raise_for_status()
            text = THINK_RE.sub("", r.json()["choices"][0]["message"]["content"]).strip()
            if text:
                return text
            last = "empty"
        except Exception as e:  # noqa: BLE001
            last = str(e)
        time.sleep(5 * (attempt + 1))
    raise RuntimeError(f"chat failed: {last}")


def parse_json(text: str) -> dict | None:
    start = text.find("{")
    if start < 0:
        return None
    depth = 0
    for i, ch in enumerate(text[start:], start):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                try:
                    return json.loads(text[start : i + 1])
                except json.JSONDecodeError:
                    return None
    return None


def chat_json(system: str, user: str, max_tokens: int) -> dict | None:
    for nudge in ("", "\n\nReply with ONLY valid JSON."):
        obj = parse_json(chat(system, user + nudge, max_tokens))
        if obj is not None:
            return obj
    return None


def doc_header(file_path: str, max_lines: int = 25) -> tuple[str, int]:
    """Return (header text, number of source lines scanned for it)."""
    lines, n = [], 0
    try:
        with open(REPO / file_path, encoding="utf-8", errors="replace") as fh:
            for idx, ln in enumerate(fh, 1):
                s = ln.strip()
                if s.startswith("//!"):
                    lines.append(s[3:].strip())
                    n = idx
                    if len(lines) >= max_lines:
                        break
                elif lines or (s and not s.startswith("//") and not s.startswith("#!")):
                    if lines:
                        break
    except OSError:
        pass
    return "\n".join(lines), n


def target_files() -> list[str]:
    nodes = json.loads(NODES.read_text())
    files = [n["path"] for n in nodes.values() if n["tier"] == "file"]
    return sorted(f for f in files if doc_header(f)[0])


def evidence_lines(file_path: str, leaves_by_file: dict) -> list[str]:
    """mod.rs / lib.rs headers describe the whole module — their evidence is
    every function under the parent directory (v1 phantom taxonomy fix #3)."""
    module_mode = Path(file_path).name in ("mod.rs", "lib.rs")
    if module_mode:
        prefix = str(Path(file_path).parent) + "/"
        kids = [e for fp, es in leaves_by_file.items() if fp.startswith(prefix) for e in es]
        kids.sort(key=lambda e: (e["meta"]["file_path"], e["meta"]["line_start"]))
    else:
        kids = sorted(leaves_by_file.get(file_path, []), key=lambda e: e["meta"]["line_start"])
    out, used = [], 0
    for k in kids:
        loc = f"{Path(k['meta']['file_path']).name}::" if module_mode else ""
        line = f"{loc}{k['meta']['name']} (line {k['meta']['line_start']}): {k['summary'][:200]}"
        if used + len(line) > EVIDENCE_CHAR_BUDGET:
            out.append(f"[... {len(kids) - len(out)} more functions omitted for length ...]")
            break
        out.append(line)
        used += len(line)
    return out


def load_ckpt(name: str) -> dict:
    p = OUT / name
    return json.loads(p.read_text()) if p.exists() else {}


def save_ckpt(name: str, data: dict) -> None:
    (OUT / name).write_text(json.dumps(data, indent=1))


# -------------------------------------------------------------------- phases


def phase_claims(limit: int | None) -> None:
    OUT.mkdir(exist_ok=True)
    done = load_ckpt("claims.json")
    files = target_files()
    if limit:
        files = files[:limit]
    log(f"claims: {len(files)} headers ({sum(1 for f in files if f in done)} cached)")
    errs = 0
    for i, f in enumerate(files):
        if f in done:
            continue
        header, hdr_lines = doc_header(f)
        obj = chat_json(SYSTEM_DECOMPOSE, f"file: {f}\nheader:\n{header}", 900)
        if obj is None or "claims" not in obj:
            errs += 1
            done[f] = {"header": header, "header_lines": hdr_lines, "claims": [], "parse_error": True}
            log(f"  PARSE ERROR (decompose) {f}")
        else:
            done[f] = {"header": header, "header_lines": hdr_lines, "claims": obj["claims"], "parse_error": False}
        if (i + 1) % 10 == 0 or i == len(files) - 1:
            save_ckpt("claims.json", done)
            log(f"claims {i + 1}/{len(files)} (parse errors {errs})")
        if errs > len(files) * 0.1 and errs > 5:
            raise RuntimeError("decompose parse-error rate >10% — aborting loudly")
    save_ckpt("claims.json", done)


def phase_adjudicate(limit: int | None) -> None:
    claims = load_ckpt("claims.json")
    done = load_ckpt("verdicts.json")
    cache = json.loads(CACHE.read_text())
    leaves_by_file = defaultdict(list)
    for e in cache:
        leaves_by_file[e["meta"]["file_path"]].append(e)

    files = [f for f in sorted(claims) if not claims[f].get("parse_error")]
    if limit:
        files = files[:limit]
    todo = [f for f in files if f not in done]
    log(f"adjudicate: {len(todo)} files to go")
    errs = 0
    for i, f in enumerate(todo):
        caps = [c for c in claims[f]["claims"] if c.get("claim_class") == "capability"]
        if not caps:
            done[f] = {"verdicts": [], "silent_growth": None, "skipped": "no capability claims"}
            continue
        numbered = "\n".join(f"{j + 1}. {c['statement']}" for j, c in enumerate(caps))
        ev = "\n".join(evidence_lines(f, leaves_by_file))
        user = f"file: {f}\nheader (for silent-growth check):\n{claims[f]['header']}\n\nclaims:\n{numbered}\n\nfunction evidence:\n{ev}"
        obj = chat_json(SYSTEM_ADJUDICATE, user, 1200)
        if obj is None or "verdicts" not in obj:
            errs += 1
            done[f] = {"verdicts": [], "silent_growth": None, "parse_error": True}
            log(f"  PARSE ERROR (adjudicate) {f}")
        else:
            for v in obj["verdicts"]:
                idx = (v.get("claim") or 0) - 1
                v["statement"] = caps[idx]["statement"] if 0 <= idx < len(caps) else "?"
            done[f] = {"verdicts": obj["verdicts"], "silent_growth": obj.get("silent_growth")}
        if (i + 1) % 10 == 0 or i == len(todo) - 1:
            save_ckpt("verdicts.json", done)
            log(f"adjudicate {i + 1}/{len(todo)} (parse errors {errs})")
    save_ckpt("verdicts.json", done)


def candidate_findings(verdicts: dict) -> list[dict]:
    out = []
    for f, rec in verdicts.items():
        for v in rec.get("verdicts", []):
            if v.get("verdict") == "contradicted":
                out.append({"kind": "drift", "file": f, "statement": v["statement"],
                            "cite": v.get("cite", []), "note": v.get("note", "")})
        sg = rec.get("silent_growth")
        if sg:
            out.append({"kind": "silent_growth", "file": f,
                        "statement": sg.get("capability", ""), "cite": sg.get("cite", []), "note": ""})
    return out


def source_at_cites(file_path: str, cites: list[str], span: int = 25) -> str:
    """Actual source at each cited fn_name:line — the verifier judges against
    code, not summaries. Module-mode cites ("sibling.rs::fn (line N)") resolve
    to the sibling file under the header file's directory."""
    blocks = []
    for cite in cites[:4]:
        cite = str(cite)
        target = file_path
        fm = re.search(r"([\w.-]+\.rs)\s*::", cite)
        if fm:
            target = str(Path(file_path).parent / fm.group(1))
        m = re.search(r"[:\s](\d+)\)?\s*$", cite) or re.search(r":(\d+)", cite)
        if not m:
            continue
        try:
            src = (REPO / target).read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        start = max(int(m.group(1)) - 1, 0)
        chunk = "\n".join(src[start : start + span])
        blocks.append(f"--- {target}:{start + 1} ---\n{chunk}")
    return "\n".join(blocks) if blocks else "(no parseable cite lines)"


def phase_verify() -> None:
    verdicts = load_ckpt("verdicts.json")
    claims = load_ckpt("claims.json")
    done = load_ckpt("verify.json")

    cands = candidate_findings(verdicts)
    log(f"verify: {len(cands)} candidate findings")
    for c in cands:
        key = f"{c['kind']}|{c['file']}|{c['statement'][:80]}"
        if key in done:
            continue
        if c["kind"] == "drift":
            finding = (f"The doc header claims: \"{c['statement']}\"\n"
                       f"The adjudicator found the code CONTRADICTS this claim. "
                       f"Adjudicator's reasoning: {c['note']}\nCited: {c['cite']}")
        else:
            finding = (f"The doc header never mentions this capability, which the adjudicator "
                       f"says the file substantially implements: \"{c['statement']}\"\nCited: {c['cite']}")
        user = (f"file: {c['file']}\n\ndoc header:\n{claims[c['file']]['header']}\n\n"
                f"proposed finding:\n{finding}\n\n"
                f"actual source at cited lines:\n{source_at_cites(c['file'], c['cite'])}")

        # Polarity guard (v1 taxonomy fix #2): the boolean and the reason text
        # must agree; one retry, then conservative kill logged as a conflict.
        verdict, conflict = None, False
        for attempt in range(2):
            obj = chat_json(SYSTEM_VERIFY, user, 400)
            if obj is None:
                continue
            accurate = bool(obj.get("header_accurate", True))
            reason = str(obj.get("reason", ""))
            pol = chat_json(SYSTEM_POLARITY, f"sentence: {reason}", 100) or {}
            says = pol.get("reason_says", "unclear")
            agree = (not accurate and says == "header_wrong") or (accurate and says == "header_fine")
            if agree or says == "unclear":
                verdict = {"refuted": accurate, "reason": reason,
                           "code_shows": obj.get("code_shows", ""),
                           "header_says": obj.get("header_says", ""),
                           "polarity": says}
                break
            log(f"  POLARITY CONFLICT (attempt {attempt + 1}) {c['file']}: accurate={accurate} but reason_says={says}")
            conflict = True
        if verdict is None:
            verdict = {"refuted": True, "reason": "unresolved polarity conflict or parse failure -> killed",
                       "polarity": "conflict"}
        verdict["polarity_conflict_seen"] = conflict
        done[key] = {**c, **verdict}
        save_ckpt("verify.json", done)
        log(f"  {c['kind']} {c['file']}: ship={not verdict['refuted']}")
    save_ckpt("verify.json", done)


def header_age(file_path: str, n_lines: int) -> str:
    try:
        r = subprocess.run(
            ["git", "log", "-1", "--format=%as", "-L", f"1,{max(n_lines, 1)}:{file_path}"],
            cwd=REPO, capture_output=True, text=True, timeout=30,
        )
        for ln in r.stdout.splitlines():
            if re.match(r"\d{4}-\d{2}-\d{2}", ln):
                return ln.strip()
    except Exception:  # noqa: BLE001
        pass
    return "?"


# v1 known-real regression set (README v2 gate V2): these five must ship again.
REGRESSION_SET = [
    ("enrichment/atlas/section_cache.rs", "sha256"),
    ("enrichment/atlas/section_cache.rs", "lookup"),
    ("enrichment/domains/multi.rs", "todo"),
    ("update/watch.rs", "codewatcher"),
    ("enrichment/atlas/analysis/tensions.rs", "signal"),
]


def phase_report() -> None:
    claims = load_ckpt("claims.json")
    verdicts = load_ckpt("verdicts.json")
    verify = load_ckpt("verify.json")

    class_counts = Counter(c.get("claim_class", "unparsed") for rec in claims.values() for c in rec["claims"])
    verdict_counts = Counter(v.get("verdict", "unparsed") for rec in verdicts.values() for v in rec.get("verdicts", []))
    shipped = [v for v in verify.values() if not v["refuted"]]
    killed = [v for v in verify.values() if v["refuted"]]

    lines = ["# Header-reconcile report\n",
             f"headers decomposed: {len(claims)} | files adjudicated: {len(verdicts)}",
             f"claims by class: {dict(class_counts)}",
             f"capability verdicts: {dict(verdict_counts)}",
             f"candidates: {len(verify)} | shipped: {len(shipped)} | killed by verify: {len(killed)}\n",
             "## Shipped findings (drift + silent growth)\n"]
    for s in sorted(shipped, key=lambda x: (x["kind"], x["file"])):
        age = header_age(s["file"], claims[s["file"]]["header_lines"])
        lines += [f"### [{s['kind']}] {s['file']}  (header last touched {age})",
                  f"- finding: {s['statement']}",
                  f"- cited: {', '.join(s['cite']) if s['cite'] else '(none)'}",
                  f"- adjudicator note: {s['note']}",
                  f"- header excerpt: {claims[s['file']]['header'][:300]}", ""]
    lines.append("## Killed by adversarial verify\n")
    for s in killed:
        lines.append(f"- [{s['kind']}] {s['file']}: {s['statement'][:120]} — {s['reason']}")

    conflicts = sum(1 for v in verify.values() if v.get("polarity_conflict_seen"))
    recall = []
    for file_sub, stmt_sub in REGRESSION_SET:
        hit = any(s["file"].endswith(file_sub) and stmt_sub in s["statement"].lower() for s in shipped)
        recall.append((file_sub, stmt_sub, hit))
    lines += ["\n## v2 gates", f"- polarity conflicts seen: {conflicts}",
              f"- regression-set recall: {sum(1 for *_, h in recall if h)}/5"]
    for file_sub, stmt_sub, hit in recall:
        lines.append(f"  - [{'SHIPPED' if hit else 'MISSING'}] {file_sub} ~ '{stmt_sub}'")
    (OUT / "report.md").write_text("\n".join(lines))
    log(f"report.md written: {len(shipped)} shipped, {len(killed)} killed, "
        f"{conflicts} polarity conflicts, recall {sum(1 for *_, h in recall if h)}/5")
    log(f"base rate: {dict(verdict_counts)} over {len(claims)} headers")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("phase", choices=["claims", "adjudicate", "verify", "report", "all"])
    ap.add_argument("--limit", type=int, default=None)
    ap.add_argument("--out", default="out", help="checkpoint dir under this folder (v2: out-v2)")
    args = ap.parse_args()
    OUT = HERE / args.out
    if args.phase in ("claims", "all"):
        phase_claims(args.limit)
    if args.phase in ("adjudicate", "all"):
        phase_adjudicate(args.limit)
    if args.phase in ("verify", "all"):
        phase_verify()
    if args.phase in ("report", "all"):
        phase_report()
