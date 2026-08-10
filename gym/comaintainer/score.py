#!/usr/bin/env python3
"""Score a (model, charter) pair against the comaintainer bank.

Metrics (§18.2 — four verdicts, not two; malformed is never folded
into disagreement):

  exact-6    parsed verdict == expected verdict (HARD headline is the
             tier-A holdout number, Wilson CI)
  coarse-3   LAND {approve} / BOUNCE {revise, split} /
             DEFER {measure-first, escalate, could-not-judge}
  basis      TWO numbers, never blended: `basis-exists` (every cited
             anchor resolves) and `basis-bears` (some cited anchor
             matches the expected basis or the episode's provenance)
  malformed  malformed_no_json / malformed_bad_verdict /
             malformed_missing_arg — its own outcome class

Instrumented per ARCH §18.4's corollary: every run persists FULL raw
completions under `runs/<stamp>/` and `--rescore <run>` reproduces the
metrics with ZERO model calls — deliberately fixing the next-edit
mirror's known gap (note e5c02e64). `wilson` is imported from the
mirror, not re-implemented (§10.6).

Engines:
  --engine daemon   (default) POST /v1/chat/completions at temp 0.
                    Deterministic on this stack (measured 2026-08-06);
                    the charter+contract prefix rides the prefix cache.
  --engine claude   `claude -p` from a NEUTRAL cwd. Budgeted: hard cap
                    via --max-calls (default 190, the pass-level cap).

    python3 gym/comaintainer/score.py --charter none               # baseline
    python3 gym/comaintainer/score.py --charter gym/comaintainer/CHARTER.md
    python3 gym/comaintainer/score.py --rescore gym/comaintainer/runs/<stamp>
    python3 gym/comaintainer/score.py --constant revise            # floor
"""

from __future__ import annotations

import argparse
import collections
import datetime
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent.parent / "gym" / "next-edit" / "golden"))

import markers as M  # noqa: E402
from score_golden import wilson  # noqa: E402  (one formula, one home)
from validate_episodes import arch_sections, ledger_slugs  # noqa: E402

DAEMON = "http://localhost:9741/v1/chat/completions"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_prompt(ep: dict, charter_text: str | None, contract_text: str) -> str:
    parts = []
    if charter_text:
        parts.append(charter_text.strip())
    parts.append("=== OUTPUT CONTRACT ===\n" + contract_text.strip())
    r = ep["request"]
    parts.append(
        f"=== LANDING UNDER REVIEW (case {ep['id']}) ===\n"
        f"SITUATION:\n{r['situation']}\n\n"
        f"PROPOSAL:\n{r['proposal']}\n\n"
        f"EVIDENCE:\n{r['evidence']}")
    parts.append("Return the verdict JSON now.")
    return "\n\n".join(parts)


# ---- engines ----------------------------------------------------------


def call_daemon(prompt: str, timeout: float, max_tokens: int,
                schema: dict = None, schema_name: str = "verdict"
                ) -> tuple[str, str]:
    """-> (completion_text, model_id)

    `schema` is optional and OFF for gym runs on purpose. The gym measures
    the charter, and part of what a charter has to buy is a reply that
    obeys the output contract without a grammar holding the pen — force it
    here and the malformed column stops measuring anything. The SEAT is
    the other caller (scripts/co-review.sh) and it passes
    markers.verdict_schema(), because a live landing verdict is a decision,
    not a measurement: there, a malformed reply is pure loss.
    """
    body = {
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0,
        "max_tokens": max_tokens,
    }
    if schema is not None:
        # OpenAI-compatible envelope; the daemon extracts .json_schema.schema
        # and hands it to llguidance (sovereign-mesh inference_adapter.rs
        # extract_response_format_schema).
        body["response_format"] = {
            "type": "json_schema",
            "json_schema": {"name": schema_name, "schema": schema,
                            "strict": True},
        }
    req = urllib.request.Request(
        DAEMON,
        data=json.dumps(body).encode(),
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        payload = json.loads(r.read())
    text = (payload.get("choices") or [{}])[0].get("message", {}).get("content", "")
    return text, payload.get("model", "unknown")


NEUTRAL_CWD = Path(tempfile.gettempdir()) / "co-score-neutral"


def call_claude(prompt: str, timeout: float, model: str | None) -> tuple[str, str]:
    """`claude -p` from a neutral cwd (never the repo — CLAUDE.md and
    boot hooks must not ride the judgment)."""
    NEUTRAL_CWD.mkdir(parents=True, exist_ok=True)
    cmd = ["claude", "-p", "--output-format", "json"]
    if model:
        cmd += ["--model", model]
    r = subprocess.run(cmd, input=prompt.encode(), capture_output=True,
                       timeout=timeout, cwd=str(NEUTRAL_CWD))
    try:
        envelope = json.loads(r.stdout.decode())
    except Exception:
        raise RuntimeError(f"claude envelope unparseable: "
                           f"{r.stdout[:200]!r} stderr={r.stderr[:200]!r}")
    if envelope.get("is_error"):
        raise RuntimeError(f"claude transport error: {str(envelope)[:300]}")
    return envelope.get("result", ""), envelope.get("model", model or "claude-default")


# ---- extraction (strict; NO retry — a retry is best-of-undeclared-n) --


def extract_verdict(completion: str) -> tuple[dict | None, str | None]:
    """-> (parsed, malformed_reason)"""
    text = re.sub(r"```(?:json)?", "", completion)
    start = text.find("{")
    if start < 0:
        return None, "malformed_no_json"
    depth = 0
    end = None
    in_str = False
    esc = False
    for i, ch in enumerate(text[start:], start):
        if esc:
            esc = False
            continue
        if ch == "\\":
            esc = True
            continue
        if ch == '"' and not esc:
            in_str = not in_str
            continue
        if in_str:
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                end = i + 1
                break
    if end is None:
        return None, "malformed_no_json"
    try:
        obj = json.loads(text[start:end])
    except Exception:
        return None, "malformed_no_json"
    v = obj.get("verdict")
    if v not in M.VERDICTS:
        return obj, "malformed_bad_verdict"
    arg = obj.get(M.ARG_OF[v])
    if arg is None or (isinstance(arg, (str, list)) and not arg):
        return obj, "malformed_missing_arg"
    return obj, None


# ---- basis resolution -------------------------------------------------


class BasisResolver:
    def __init__(self):
        self.arch = arch_sections()
        self.slugs = ledger_slugs()
        self.notes_db = Path.home() / ".sovereign" / "notes.db"
        self._note_cache: dict[str, bool] = {}
        self._commit_cache: dict[str, bool] = {}
        self._sidecar_loaded: tuple | None = None  # lazy (sidecar,) or (None,)

    def _sidecar(self) -> dict | None:
        """This repo's standing fieldglass sidecar, or None. Resolution
        lives in markers.sidecar_path (one decider). Loaded once."""
        if self._sidecar_loaded is None:
            self._sidecar_loaded = (None,)
            p, _how = M.sidecar_path(HERE.parent.parent)
            if p is not None:
                try:
                    self._sidecar_loaded = (json.loads(p.read_text()),)
                except (OSError, json.JSONDecodeError):
                    pass
        return self._sidecar_loaded[0]

    def _field_anchor_present(self, cls: str, path: str) -> bool:
        """The (class, path) pair is present in the standing sidecar.
        Reads only the renderer's DECIDED fields — no re-derived
        thresholds (§10.6)."""
        sc = self._sidecar()
        if not sc:
            return False
        if cls == "offender":
            return any(f.get("path") == path and f.get("offender")
                       for f in sc.get("files", []))
        if cls == "bridge":
            return any(f.get("path") == path and f.get("bridge", 0) > 0
                       for f in sc.get("files", []))
        att = sc.get("attention", {})
        if cls == "tollbooth":
            return any(t and t[0] == path for t in att.get("tollbooths", []))
        if cls == "tax":
            return any(d.get("path") == path
                       for d in att.get("comprehension_tax", []))
        if cls == "dup":
            return any(path in (a.get("a"), a.get("b"))
                       for a in sc.get("dup_arcs", []))
        if cls == "layer-violation":
            # Same violation filter as the renderer's own headline count
            # (code_fieldglass/mod.rs: kind upward | forbidden).
            crate = next((f.get("crate") for f in sc.get("files", [])
                          if f.get("path") == path), None)
            return crate is not None and any(
                e.get("kind") in M.FLOW_VIOLATION_KINDS
                and crate in (e.get("from"), e.get("to"))
                for e in sc.get("flow_edges", []))
        return False

    def exists(self, anchor: str) -> bool:
        if m := re.fullmatch(r"ARCH §(\d+(?:\.\d+)?)", anchor):
            return m.group(1) in self.arch
        if m := re.fullmatch(r"ledger:([a-z0-9\-]+)", anchor):
            return m.group(1) in self.slugs
        if m := re.fullmatch(r"note ([0-9a-f]{8})", anchor):
            h = m.group(1)
            if h not in self._note_cache:
                ok = False
                if self.notes_db.exists():
                    import sqlite3
                    with sqlite3.connect(f"file:{self.notes_db}?mode=ro",
                                         uri=True) as db:
                        ok = db.execute(
                            "SELECT 1 FROM notes WHERE id LIKE ? LIMIT 1",
                            (h + "%",)).fetchone() is not None
                self._note_cache[h] = ok
            return self._note_cache[h]
        if m := re.fullmatch(r"commit ([0-9a-f]{7,40})", anchor):
            h = m.group(1)
            if h not in self._commit_cache:
                r = subprocess.run(["git", "-C", str(HERE.parent.parent),
                                    "cat-file", "-e", h], capture_output=True)
                self._commit_cache[h] = r.returncode == 0
            return self._commit_cache[h]
        if re.fullmatch(r"transcript:[0-9a-f]{8}:\d+", anchor):
            return True  # local-only anchor; resolvable on this host by design
        if m := M.FIELD_ANCHOR_RE.fullmatch(anchor):
            # Deliberately NOT the transcript: always-True shape: an absent
            # sidecar makes the claim unverifiable here, and unverifiable
            # must not read as verified (§18.3).
            return self._field_anchor_present(m.group(1), m.group(2))
        return False

    def bears(self, anchor: str, ep: dict) -> bool:
        if anchor in ep["expect"].get("basis", []):
            return True
        # An ARCH § cite bears if the expected basis cites the same
        # top-level section (18.5 vs 18.4 is the same doctrine family).
        if m := re.fullmatch(r"ARCH §(\d+)(?:\.\d+)?", anchor):
            for b in ep["expect"].get("basis", []):
                bm = re.fullmatch(r"ARCH §(\d+)(?:\.\d+)?", b)
                if bm and bm.group(1) == m.group(1):
                    return True
        if m := re.fullmatch(r"commit ([0-9a-f]{7,40})", anchor):
            pc = ep["provenance"].get("commit") or ""
            if pc.startswith(m.group(1)) or m.group(1).startswith(pc[:7] or "\0"):
                return True
        if m := re.fullmatch(r"note ([0-9a-f]{8})", anchor):
            nid = ep["provenance"].get("note_id") or ""
            if nid.startswith(m.group(1)):
                return True
        return False


# ---- scoring ----------------------------------------------------------


def score_rows(rows: list[dict], bank_by_id: dict[str, dict]) -> None:
    """Print the metric block for a set of scored rows (used by both a
    live run and --rescore — one implementation)."""
    scored = [r for r in rows if r["id"] in bank_by_id]
    # Situated transcript episodes (operator one-offs whose correction
    # only made sense inside their session) are a tracked STEERING LANE:
    # completions are still collected and reported, but they never enter
    # the dev/holdout agreement numbers (operator decision 2026-08-06).
    situated = [r for r in scored
                if bank_by_id[r["id"]].get("scope") == "situated"]
    core = [r for r in scored
            if bank_by_id[r["id"]].get("scope") != "situated"]
    for label, subset in (
        ("TIER-A HOLDOUT (the HARD headline)",
         [r for r in core
          if bank_by_id[r["id"]]["tier"] == "A"
          and bank_by_id[r["id"]]["split"] == "holdout"]),
        ("ALL SCORED (situated steering lane excluded)", core),
        ("STEERING LANE — situated transcript (tracked, never gated)",
         situated),
    ):
        if label.startswith("STEERING") and not subset:
            continue  # bank predates the scope flag, or lane not in split
        if not subset:
            print(f"\n{label}: EMPTY — nothing to report (§18.2, not a pass)")
            continue
        n = len(subset)
        present = collections.Counter(
            bank_by_id[r["id"]]["expect"]["verdict"] for r in subset)
        empty = [v for v in M.VERDICTS if present.get(v, 0) == 0]
        if empty:
            print(f"\n{label}: HOLDOUT-EMPTY CLASSES (named before the "
                  f"headline, §18.2): {empty}")
        exact = sum(1 for r in subset if r.get("agree_exact"))
        coarse = sum(1 for r in subset if r.get("agree_coarse"))
        malformed = collections.Counter(
            r["malformed"] for r in subset if r.get("malformed"))
        nm = sum(malformed.values())
        lo, hi = wilson(exact, n)
        print(f"\n{label}  (n={n})")
        print(f"  exact-6  {exact}/{n} = {100*exact/n:.1f}%  "
              f"(95% CI {100*lo:.1f}–{100*hi:.1f}%)")
        clo, chi = wilson(coarse, n)
        print(f"  coarse-3 {coarse}/{n} = {100*coarse/n:.1f}%  "
              f"(95% CI {100*clo:.1f}–{100*chi:.1f}%)")
        if nm:
            print(f"  malformed {nm}/{n} = {100*nm/n:.1f}%  {dict(malformed)}"
                  f"  — NOT folded into disagreement; rates above are "
                  f"conditioned on a well-formed reply existing")
        bx = [r for r in subset if r.get("basis_cited")]
        if bx:
            be = sum(1 for r in bx if r["basis_exists"])
            bb = sum(1 for r in bx if r["basis_bears"])
            print(f"  basis-exists {be}/{len(bx)} = {100*be/len(bx):.1f}%   "
                  f"basis-bears {bb}/{len(bx)} = {100*bb/len(bx):.1f}%   "
                  f"(over {len(bx)} rows that cited anything; two numbers, "
                  f"never blended)")
        # confusion + per-class recall
        conf: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
        for r in subset:
            want = bank_by_id[r["id"]]["expect"]["verdict"]
            got = r.get("parsed_verdict") or f"<{r.get('malformed')}>"
            conf[want][got] += 1
        # Hoisted out of the f-string deliberately: a backslash inside an
        # f-string EXPRESSION is a SyntaxError before Python 3.12, and this
        # file is imported by scripts/co-review.sh, which runs under
        # launchd's /usr/bin/python3 (3.9 on macOS). The file must parse on
        # the OLDEST interpreter that loads it, not the newest one on PATH.
        conf_header = "expect \\ got"
        print(f"  {conf_header:<18}"
              + "".join(f"{v[:9]:>10}" for v in M.VERDICTS)
              + f"{'malformed':>10}  recall")
        for want in M.VERDICTS:
            row = conf.get(want, collections.Counter())
            tot = sum(row.values())
            if not tot:
                continue
            cells = "".join(f"{row.get(v, 0):>10}" for v in M.VERDICTS)
            mal = sum(c for k, c in row.items() if k.startswith("<"))
            rec = row.get(want, 0) / tot
            print(f"  {want:<18}{cells}{mal:>10}  {100*rec:.0f}%")
        walls = sorted(r["wall_ms"] for r in subset if r.get("wall_ms"))
        if walls:
            p95 = walls[max(0, min(len(walls) - 1, round(0.95 * (len(walls) - 1))))]
            print(f"  latency p50 {walls[len(walls)//2]:.0f} ms · p95 {p95:.0f} ms")


def judge_row(ep: dict, completion: str, resolver: BasisResolver,
              wall_ms: float | None, engine: str, err: str | None) -> dict:
    row = {"id": ep["id"], "raw": completion, "engine": engine,
           "wall_ms": wall_ms, "envelope_error": err,
           "parsed_verdict": None, "malformed": None,
           "agree_exact": False, "agree_coarse": False,
           "basis_cited": False, "basis_exists": False, "basis_bears": False}
    if err:
        row["malformed"] = "transport_error"
        return row
    parsed, malformed = extract_verdict(completion)
    row["malformed"] = malformed
    if parsed and not malformed:
        got = parsed["verdict"]
        row["parsed_verdict"] = got
        want = ep["expect"]["verdict"]
        row["agree_exact"] = got == want
        row["agree_coarse"] = M.COARSE_OF[got] == M.COARSE_OF[want]
        basis = parsed.get("basis") or []
        if isinstance(basis, list) and basis:
            row["basis_cited"] = True
            row["basis_exists"] = all(
                resolver.exists(str(b)) for b in basis)
            row["basis_bears"] = any(
                resolver.bears(str(b), ep) for b in basis)
    return row


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default=str(HERE / "cases.jsonl.gz"))
    ap.add_argument("--charter", default=str(HERE / "CHARTER.md"),
                    help="path, or 'none' for the charter-less baseline "
                         "(the contract is NEVER omitted — it is the ruler)")
    ap.add_argument("--contract", default=str(HERE / "contract.txt"))
    ap.add_argument("--engine", choices=("daemon", "claude"), default="daemon")
    ap.add_argument("--model", default=None, help="claude engine model override")
    ap.add_argument("--split", choices=("holdout", "dev", "all"), default="holdout")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--timeout", type=float, default=300.0)
    ap.add_argument("--max-tokens", type=int, default=700)
    ap.add_argument("--max-calls", type=int, default=190,
                    help="hard budget for --engine claude (pass-level cap)")
    ap.add_argument("--constant", default=None, metavar="VERDICT",
                    help="analytic constant-verdict floor; no model calls")
    ap.add_argument("--allow-engine-drift", action="store_true",
                    help="score against a daemon model that is NOT the "
                         "engine of record (markers.ENGINE_OF_RECORD); the "
                         "substitution is stamped into meta.json")
    ap.add_argument("--rescore", default=None, metavar="RUN_DIR",
                    help="recompute metrics from a persisted run; zero calls")
    ap.add_argument("--runs-dir", default=str(HERE / "runs"))
    args = ap.parse_args()

    bank = M.read_bank(args.cases)
    if not bank:
        print("EMPTY BANK (exit 4, not a pass)")
        sys.exit(4)
    bank_by_id = {e["id"]: e for e in bank}

    if args.constant:
        if args.constant not in M.VERDICTS:
            sys.exit(f"unknown verdict {args.constant!r}")
        for split in ("holdout", "all"):
            eps = [e for e in bank
                   if split == "all" or e["split"] == split]
            ta = [e for e in eps if e["tier"] == "A" and e["split"] == "holdout"]
            for label, subset in ((f"tier-A holdout", ta), (f"{split}", eps)):
                if not subset:
                    continue
                k = sum(1 for e in subset
                        if e["expect"]["verdict"] == args.constant)
                print(f"constant-'{args.constant}' on {label}: {k}/{len(subset)} "
                      f"= {100*k/len(subset):.1f}% exact-6")
            break
        return

    resolver = BasisResolver()

    if args.rescore:
        run_dir = Path(args.rescore)
        meta = json.loads((run_dir / "meta.json").read_text())
        rows_raw = [json.loads(l) for l in
                    (run_dir / "rows.jsonl").read_text().splitlines() if l.strip()]
        if meta.get("bank_sha256") != sha256(Path(args.cases)):
            print("WARNING: bank sha mismatch — this run was scored against a "
                  "DIFFERENT bank; metrics below are against the run's own "
                  "recorded episodes where ids still match.")
        rows = [judge_row(bank_by_id[r["id"]], r["raw"], resolver,
                          r.get("wall_ms"), r.get("engine", "?"),
                          r.get("envelope_error"))
                for r in rows_raw if r["id"] in bank_by_id]
        print(f"rescore of {run_dir.name}: engine={meta.get('engine')} "
              f"model={meta.get('model')} n={len(rows)} — ZERO model calls")
        score_rows(rows, bank_by_id)
        return

    charter_text = None
    charter_sha = "none"
    if args.charter != "none":
        charter_text = Path(args.charter).read_text()
        charter_sha = sha256(Path(args.charter))
    contract_text = Path(args.contract).read_text()

    eps = [e for e in bank if args.split == "all" or e["split"] == args.split]
    if args.limit:
        eps = eps[: args.limit]
    if args.engine == "claude" and len(eps) > args.max_calls:
        sys.exit(f"refusing: {len(eps)} episodes exceeds the claude budget "
                 f"of {args.max_calls} calls (--max-calls / --limit)")

    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    tag = "nocharter" if charter_text is None else "charter"
    run_dir = Path(args.runs_dir) / f"{stamp}-{args.engine}-{tag}"
    run_dir.mkdir(parents=True, exist_ok=True)

    rows = []
    model_seen = None
    with (run_dir / "rows.jsonl").open("w") as fh:
        for i, ep in enumerate(eps, 1):
            prompt = build_prompt(ep, charter_text, contract_text)
            t0 = time.monotonic()
            err = None
            completion = ""
            try:
                if args.engine == "daemon":
                    completion, model_seen = call_daemon(
                        prompt, args.timeout, args.max_tokens)
                else:
                    completion, model_seen = call_claude(
                        prompt, args.timeout, args.model)
            except Exception as e:
                err = f"{type(e).__name__}: {e}"
            wall = (time.monotonic() - t0) * 1000
            # Engine-of-record gate, checked on the FIRST reply: a daemon
            # restart can silently swap the resident model, and a full
            # run on the wrong judge is a wasted hour that LOOKS like a
            # regression (happened 2026-08-06). Refuse, or proceed only
            # with the substitution named on the tin (§18.3).
            if (i == 1 and args.engine == "daemon" and model_seen
                    and M.ENGINE_OF_RECORD not in model_seen):
                if not args.allow_engine_drift:
                    sys.exit(f"REFUSING: daemon serves {model_seen!r}, engine "
                             f"of record is *{M.ENGINE_OF_RECORD}* — restore "
                             f"the primary or pass --allow-engine-drift "
                             f"(run dir {run_dir} abandoned after 1 call)")
                print(f"!! ENGINE DRIFT (named, not silent): scoring against "
                      f"{model_seen!r}, NOT the engine of record "
                      f"*{M.ENGINE_OF_RECORD}* — numbers from this run are "
                      f"not comparable to committed results", file=sys.stderr)
            row = judge_row(ep, completion, resolver, wall, args.engine, err)
            rows.append(row)
            fh.write(json.dumps(row, ensure_ascii=False) + "\n")
            fh.flush()
            state = row["parsed_verdict"] or row["malformed"]
            print(f"  [{i}/{len(eps)}] {ep['id']}: {state} "
                  f"({wall:.0f} ms)", file=sys.stderr)

    meta = {
        "bank_sha256": sha256(Path(args.cases)),
        "charter": args.charter, "charter_sha256": charter_sha,
        "contract_sha256": sha256(Path(args.contract)),
        "engine": args.engine, "model": model_seen,
        "engine_of_record": M.ENGINE_OF_RECORD,
        "engine_drift": bool(args.engine == "daemon" and model_seen
                             and M.ENGINE_OF_RECORD not in model_seen),
        "split": args.split, "n": len(eps),
        "argv": sys.argv[1:], "stamp": stamp,
    }
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=2))
    print(f"\nrun persisted -> {run_dir}  (rescore with --rescore {run_dir})")

    nerr = sum(1 for r in rows if r["envelope_error"])
    if nerr:
        print(f"\n!! {nerr} TRANSPORT ERRORS — rates below are deflated; this "
              f"run is NOT comparable to a clean baseline (§18.3).")
    score_rows(rows, bank_by_id)


if __name__ == "__main__":
    main()
