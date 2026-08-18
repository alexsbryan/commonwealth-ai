#!/usr/bin/env python3
"""T5A phase-3 RACE scorer — the benchmark's own evaluator, executed locally.

Runs the exact official RACE recipe (deepresearch_bench_race.py at the pinned
clone 469cce54) against OUR artifacts, with OUR judge:

  format_criteria_list(criteria_data)                        (driver :33-56)
  → en_merged_score_prompt.format(task_prompt, article_1,
      article_2, criteria_list)                              (score_prompt_en.py)
  → judge call via the vendored OpenAI-compat client
      (vendor/utils/api.py: LLM_BACKEND=openai,
       OPENAI_BASE_URL=http://127.0.0.1:9741/v1, RACE_MODEL=<122B pin>)
  → extract_json_from_markdown + json.loads + expected dims  (driver :121-133)
  → calculate_weighted_scores (vendored score_calculator.py)
  → overall = target_total/(target_total+reference_total),
    normalized_dims[dim] = t_d/(t_d+r_d)                     (driver :155-175)
  → per-task record {id, prompt, 4 dims, overall_score}      (driver :187-195)
  → task means ×100 (race_result.txt shape)                  (driver :490-514)

Two arms (--arm local | ab | both):

  local — article_1 = the landed report.md of t4a's demo12 local-arm flights
          (demo12/runs/local/drb-<id>/dr-<ts>/, landed gate: the dr-* dir
          carries verdict-set.json — the t4a amendment-2a gate). Uncleaned,
          the report IS the deliverable (cleaning caveat D-F-4).
  ab    — article_1 = the staged official perplexity raw articles
          (inputs/perplexity-subset-articles.jsonl, sha256 b1ce5783…).
          Uncleaned (the official cleaned targets are NOT shipped — space has
          no cleaned_data; named caveat §18.6 item 6).
  hybrid — article_1 = the landed report.md of t4a's demo12 HYBRID-arm flights
          (demo12/runs/hybrid/drb-<id>/dr-<ts>/, same landed gate, same
          charter check). The web-leg arm of the same re-flight; uncleaned,
          the same caveats. (T5a-hybrid declaration, operator resolve
          2026-08-18.) --landed-root + --arm-label re-brand it for the t6a
          deep arm (T6a declaration, operator resolve 2026-08-18.)

Both arms share article_2 (the shipped cleaned reference articles) and the
shipped per-task criteria (clone data/…, frozen at the pin).

Judge guard (real mode only): before ANY judge call, GET {base}/models and
require the pinned RACE_MODEL to be listed. A wrong/missing judge refuses
loudly (exit 2) — the flight never runs against a substitute (§18.3). Dry-run
reports the served model but does not require it.

Dry-run (--dry-run): builds every prompt, asserts every linkage (clone pin,
criteria/reference rows, landed-flight gate, charter question == frozen
prompt, ab ids/prompts == frozen, input sha256), prints per-task input sizes
and the total token estimate. NO judge call.

Resume (--resume FLIGHT_DIR): after a crash, the persisted judge_output.jsonl
per arm IS the evidence — an arm whose sidecar covers all 10 tasks with our
judge's output is re-derived from disk (ZERO judge calls; the derivation is
deterministic and identical to the fresh path — same compute_record); every
other arm runs fresh. Refusals (incomplete sidecar, wrong judge identity) are
named, never silent (§18.3).

Retry (--resume FLIGHT_DIR --retry TASK_ID): a single-task re-run for an arm
that lost exactly one task to a transient failure (e.g. 503 queue-full). ONE
fresh judge call for TASK_ID, merged with the persisted sidecar (which must
cover the other 9 tasks with our judge's output), then the FULL arm is
re-derived through compute_record — the one decider — and written complete.
Refusals (task already scored, coverage mismatch, wrong judge) are named,
never silent.

Every number produced carries the judge-identity caveat (§18.6 item 1): ours
is a different model from the official judges (gemini-2.5-pro / GPT-5.5 era).
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
DRB = HERE.parent
REPO = DRB.parent                       # research/deep-research
DEMO12_LOCAL = REPO / "demo" / "demo12" / "runs" / "local"
DEMO12_HYBRID = REPO / "demo" / "demo12" / "runs" / "hybrid"
CLONE = Path("/home/alexbryan/dev/deep_research_bench")
PIN = "469cce54ea7f6a63c163d3d9fec879cf289ec484"

SUBSET_IDS = [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]
SUBSET_ARTICLES_SHA = "b1ce57831916bd0e487b8816d3ef6b3fe3c3cb1ce73e26cdce4d6e9da4f3b0e7"
DIMS = ["comprehensiveness", "insight", "instruction_following", "readability"]
JUDGE_PIN = "Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003"   # the seat's 122B
MAX_RETRIES = int(os.environ.get("MAX_RETRIES", "10"))       # official recipe

# ---- sources -----------------------------------------------------------

def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_jsonl(path: Path):
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def check_clone() -> None:
    if not (CLONE / "deepresearch_bench_race.py").exists():
        sys.exit(f"exit 3: pinned clone missing at {CLONE}")
    out = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=CLONE, capture_output=True, text=True
    ).stdout.strip()
    if out != PIN:
        sys.exit(f"exit 3: clone not at pin {PIN} (found {out})")


# import the official recipe from the pinned clone (read-only, never edited)
sys.path.insert(0, str(CLONE))
from deepresearch_bench_race import format_criteria_list          # noqa: E402
from prompt.score_prompt_en import (                              # noqa: E402
    generate_merged_score_prompt as en_merged_score_prompt,
)
sys.path.insert(0, str(DRB / "vendor"))                            # vendored
from utils.api import AIClient                                    # noqa: E402
from utils.json_extractor import extract_json_from_markdown       # noqa: E402
from utils.score_calculator import calculate_weighted_scores      # noqa: E402

# byte-identity: vendored copies == the clone's (vendored verbatim)
for name in ("score_calculator.py", "json_extractor.py", "api.py"):
    if sha256_of(DRB / "vendor" / "utils" / name) != sha256_of(
        CLONE / "utils" / name
    ):
        sys.exit(f"exit 3: vendored {name} differs from the pinned clone")


# ---- inputs ------------------------------------------------------------

def frozen_prompts() -> dict:
    rows = load_jsonl(DRB / "query.subset.jsonl")
    ids = [r["id"] for r in rows]
    assert ids == SUBSET_IDS, f"query.subset ids {ids} != {SUBSET_IDS}"
    return {r["id"]: r["prompt"] for r in rows}


def criteria_by_id() -> dict:
    rows = load_jsonl(CLONE / "data" / "criteria_data" / "criteria.jsonl")
    return {r["id"]: r for r in rows}


def reference_by_id() -> dict:
    rows = load_jsonl(CLONE / "data" / "test_data" / "cleaned_data" / "reference.jsonl")
    return {r["id"]: r["article"] for r in rows}


def landed_report(task_id: int, root: Path) -> Path:
    """The ONE landed report.md for a task in a demo12 arm root (amendment-2a
    gate: verdict-set.json present). Refuses if zero or multiple landed dirs —
    never guesses. The root names the arm: demo12/runs/{local,hybrid}/."""
    task_root = root / f"drb-{task_id}"
    landed = [
        d for d in task_root.glob("dr-*")
        if (d / "verdict-set.json").exists() and (d / "report.md").exists()
    ]
    if len(landed) != 1:
        sys.exit(
            f"exit 3: task {task_id}: expected exactly 1 landed flight in "
            f"{task_root}, found {len(landed)}: {[d.name for d in landed]}"
        )
    return landed[0] / "report.md"


def subset_articles() -> dict:
    f = HERE / "inputs" / "perplexity-subset-articles.jsonl"
    got = sha256_of(f)
    if got != SUBSET_ARTICLES_SHA:
        sys.exit(f"exit 3: subset-articles sha256 {got[:12]}… != pinned b1ce5783…")
    rows = load_jsonl(f)
    return {r["id"]: r for r in rows}


# ---- the official recipe, executed --------------------------------------

def build_prompt(task_prompt: str, article_1: str, article_2: str,
                 criteria_data: dict) -> str:
    criteria_list_str = format_criteria_list(criteria_data)
    return en_merged_score_prompt.format(
        task_prompt=task_prompt,
        article_1=article_1,
        article_2=article_2,
        criteria_list=criteria_list_str,
    )


def served_models() -> dict:
    """{model_id: loaded_bool} — the daemon's own load truth
    (performance.loaded, the T3c (c0) surface the seat's window protocol
    manages). A registered-but-unloaded model is NOT served."""
    import requests
    base = os.environ.get("OPENAI_BASE_URL", "http://127.0.0.1:9741/v1").rstrip("/")
    r = requests.get(f"{base}/models", timeout=30)
    r.raise_for_status()
    return {m.get("id"): bool(m.get("performance", {}).get("loaded"))
            for m in r.json().get("data", [])}


def judge_call(client: AIClient, prompt: str, task_id: int) -> dict:
    """One task's judge call with the official retry recipe (10 × 1.5^retry
    backoff). Returns the parsed, dim-complete JSON."""
    last_err = None
    for retry in range(MAX_RETRIES):
        try:
            raw = client.generate(user_prompt=prompt, system_prompt="")
            extracted = extract_json_from_markdown(raw)
            if not extracted:
                raise ValueError("no JSON extracted from judge response")
            out = json.loads(extracted)
            missing = [d for d in DIMS if d not in out]
            if missing:
                raise ValueError(f"missing expected dimensions: {missing}")
            return out
        except Exception as e:                      # noqa: BLE001 — official recipe
            last_err = e
            if retry + 1 < MAX_RETRIES:
                time.sleep(1.5 ** (retry + 1))
    raise RuntimeError(f"task {task_id}: judge failed after {MAX_RETRIES} "
                       f"retries — {last_err}")


def compute_record(task_id: int, out: dict, task_prompt: str,
                   criteria: dict) -> dict:
    """The official scoring math — ONE implementation shared by the fresh and
    the resume path (one decider, §10.6): calculate_weighted_scores → overall
    ratio → normalized dims → the record shape of the official driver
    (:155-195)."""
    criteria_data = criteria[task_id]
    # criteria weights sanity (one decider — already asserted by the verifier)
    dw = criteria_data["dimension_weight"]
    assert abs(sum(dw.values()) - 1.0) < 1e-9, f"id {task_id} dimension_weight"
    for dim, crits in criteria_data["criterions"].items():
        assert abs(sum(c["weight"] for c in crits) - 1.0) < 1e-9, f"id {task_id} {dim}"
    scores = calculate_weighted_scores(out, criteria_data)
    t_total = scores["target"]["total"]
    r_total = scores["reference"]["total"]
    overall = t_total / (t_total + r_total) if t_total + r_total > 0 else 0.0
    normalized = {}
    for dim in DIMS:
        t_d = scores["target"]["dims"][f"{dim}_weighted_avg"]
        r_d = scores["reference"]["dims"][f"{dim}_weighted_avg"]
        normalized[dim] = t_d / (t_d + r_d) if t_d + r_d > 0 else 0.0
    return {"id": task_id, "prompt": task_prompt, **normalized,
            "overall_score": overall}


def score_task(task_id: int, prompts: dict, criteria: dict, references: dict,
               article_1: str, client: AIClient | None, dry_run: bool,
               sidecar: list) -> dict | None:
    task_prompt = prompts[task_id]
    prompt = build_prompt(task_prompt, article_1, references[task_id],
                          criteria[task_id])

    if dry_run:
        print(f"  id {task_id}: prompt {len(prompt):>7} chars "
              f"(article_1 {len(article_1):>6})")
        return None

    t0 = time.time()
    out = judge_call(client, prompt, task_id)
    t1 = time.time()
    record = compute_record(task_id, out, task_prompt, criteria)
    sidecar.append({
        "id": task_id, "start_unix": round(t0, 1), "end_unix": round(t1, 1),
        "elapsed_s": round(t1 - t0, 1), "judge_model": JUDGE_PIN,
        "judge_output": out,
    })
    print(f"  id {task_id}: overall={record['overall_score']:.4f} "
          f"({(t1 - t0)/60:.1f} min, {len(out)} dims parsed)")
    return record


def run_arm(name: str, article_1s: dict, prompts: dict, criteria: dict,
            references: dict, client: AIClient | None, dry_run: bool,
            out_dir: Path) -> int:
    """One arm, fresh judge calls. Returns the number of scored tasks."""
    print(f"== arm {name} (fresh judge calls) ==")
    records, sidecar = [], []
    for task_id in SUBSET_IDS:
        try:
            rec = score_task(task_id, prompts, criteria, references,
                             article_1s[task_id], client, dry_run, sidecar)
            if rec is not None:
                records.append(rec)
        except Exception as e:                       # noqa: BLE001
            # the official driver's own behavior: a failed task is an ERROR
            # record, the run continues — a transient daemon blip must not
            # abort the flight (§18.2: the failure is named, never silent)
            records.append({"id": task_id, "prompt": prompts[task_id],
                            "error": str(e)})
            print(f"  id {task_id}: ERROR — {e} (recorded, flight continues)")
    if dry_run:
        return 0
    return write_arm_outputs(name, records, sidecar, out_dir)


def write_arm_outputs(name: str, records: list, sidecar: list,
                      out_dir: Path) -> int:
    """Persist one arm's records + sidecar and compute the official 5-line
    summary (race_result.txt, means ×100 over the SCORED rows only; failed
    tasks are named beside it, never folded). Returns the number of scored
    rows. ZERO scored rows is a NAMED FAILURE — never a divide-by-zero crash,
    and race_result.txt is never written with no numbers to back it
    (four-verdicts, §18.2/§18.3)."""
    arm_dir = out_dir / name
    arm_dir.mkdir(parents=True, exist_ok=True)
    with open(arm_dir / "raw_results.jsonl", "w", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    with open(arm_dir / "judge_output.jsonl", "w", encoding="utf-8") as f:
        for s in sidecar:
            f.write(json.dumps(s, ensure_ascii=False) + "\n")
    ok = [r for r in records if "error" not in r]
    failed = [r for r in records if "error" in r]
    if failed:
        with open(arm_dir / "errors.jsonl", "w", encoding="utf-8") as f:
            for r in failed:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
    if not ok:
        print(f"  ARM {name}: FAILED — 0/{len(records)} tasks scored; "
              f"race_result.txt NOT written (no numbers exist); failed ids: "
              f"{[r['id'] for r in failed]}")
        return 0
    means = {m: sum(r[m] for r in ok) / len(ok) for m in DIMS}
    means["overall_score"] = sum(r["overall_score"] for r in ok) / len(ok)
    lines = [
        f"Comprehensiveness: {means['comprehensiveness']*100:.4f}",
        f"Insight: {means['insight']*100:.4f}",
        f"Instruction Following: {means['instruction_following']*100:.4f}",
        f"Readability: {means['readability']*100:.4f}",
        f"Overall Score: {means['overall_score']*100:.4f}",
    ]
    (arm_dir / "race_result.txt").write_text("\n".join(lines) + "\n",
                                             encoding="utf-8")
    print(f"  -> {arm_dir / 'race_result.txt'}")
    for line in lines:
        print(f"     {line}")
    print(f"  {len(ok)}/{len(records)} tasks scored"
          + (f"; failed: {[r['id'] for r in failed]}" if failed else ""))
    return len(ok)


def _sidecar_issues(name: str, rows: list) -> list:
    """Named structural issues in a persisted sidecar: duplicate ids, wrong
    judge identity, non-dict judge_output. Empty list = structurally ours."""
    issues = []
    ids = [r["id"] for r in rows]
    dups = sorted({i for i in ids if ids.count(i) > 1})
    if dups:
        issues.append(f"duplicate ids {dups}")
    bad_judge = [r["id"] for r in rows if r.get("judge_model") != JUDGE_PIN]
    if bad_judge:
        issues.append(f"wrong judge on {bad_judge}")
    bad_out = [r["id"] for r in rows
               if not isinstance(r.get("judge_output"), dict)]
    if bad_out:
        issues.append(f"non-dict judge_output on {bad_out}")
    return issues


def sidecar_derivable(name: str, sidecar_path: Path) -> bool:
    """True when the persisted sidecar covers all 10 tasks with OUR judge's
    complete output. Refusal reasons are NAMED here, never silent (§18.3)."""
    rows = load_jsonl(sidecar_path)
    issues = _sidecar_issues(name, rows)
    by_id = {r["id"]: r for r in rows}
    missing = [i for i in SUBSET_IDS if i not in by_id]
    if missing:
        issues.append(f"missing ids {missing}")
    if issues:
        print(f"  ARM {name}: persisted sidecar NOT derivable — "
              + "; ".join(issues) + " — running fresh")
        return False
    return True


def retry_sidecar_ready(name: str, sidecar_path: Path, retry_id: int) -> bool:
    """True when the persisted sidecar covers exactly the other 9 tasks with
    OUR judge's output — the precondition for a one-call retry. Refusal
    reasons are NAMED here, never silent (§18.3)."""
    rows = load_jsonl(sidecar_path)
    issues = _sidecar_issues(name, rows)
    by_id = {r["id"]: r for r in rows}
    if retry_id in by_id:
        issues.append(f"task {retry_id} already scored — nothing to retry")
    missing = [i for i in SUBSET_IDS if i not in by_id]
    if missing != [retry_id]:
        issues.append(f"coverage mismatch: missing {missing}, expected "
                      f"exactly [{retry_id}]")
    if issues:
        print(f"  ARM {name}: retry refused — " + "; ".join(issues))
        return False
    return True


def retry_task(name: str, retry_id: int, sidecar_path: Path, article_1s: dict,
               prompts: dict, criteria: dict, references: dict,
               client: AIClient, out_dir: Path) -> int:
    """One fresh judge call for retry_id, merged with the persisted sidecar
    (which must cover the other 9 tasks) and re-derived COMPLETE through
    compute_record — the one decider. A failure of the single call is LOUD
    (propagates — nothing to continue); no arm outputs are written then."""
    print(f"== arm {name} (retry task {retry_id}: 1 judge call) ==")
    rows = load_jsonl(sidecar_path)
    fresh_sidecar = []
    score_task(retry_id, prompts, criteria, references, article_1s[retry_id],
               client, False, fresh_sidecar)
    combined = sorted(rows + fresh_sidecar,
                      key=lambda r: SUBSET_IDS.index(r["id"]))
    by_id = {r["id"]: r for r in combined}
    records = [compute_record(i, by_id[i]["judge_output"], prompts[i], criteria)
               for i in SUBSET_IDS]
    scored = write_arm_outputs(name, records, combined, out_dir)
    print(f"  ARM {name}: task {retry_id} re-scored fresh; full arm "
          f"re-derived from {len(combined)} sidecar rows (1 judge call total)")
    return scored


def run_one_arm(a: str, article_1s: dict, prompts: dict, criteria: dict,
                references: dict, client: AIClient | None, dry_run: bool,
                out_dir: Path, modes: dict, args) -> int:
    """Dispatch one arm by its decided mode: derive (0 calls), retry (1
    call), fresh (10 calls). Refusals were already named at mode decision;
    the retry precondition is re-checked here before any call."""
    if modes[a] == "derive":
        return derive_from_sidecar(a, args.resume / a / "judge_output.jsonl",
                                   prompts, criteria, out_dir)
    if modes[a] == "retry":
        sp = args.resume / a / "judge_output.jsonl"
        if not retry_sidecar_ready(a, sp, args.retry):
            sys.exit(f"exit 3: retry {args.retry} refused in arm {a} — "
                     f"see above")
        return retry_task(a, args.retry, sp, article_1s, prompts, criteria,
                          references, client, out_dir)
    return run_arm(a, article_1s, prompts, criteria, references,
                   client, dry_run, out_dir)


def derive_from_sidecar(name: str, sidecar_path: Path, prompts: dict,
                        criteria: dict, out_dir: Path) -> int:
    """Re-derive an arm from a persisted judge_output.jsonl — ZERO judge
    calls (the seat's recovery order: never re-burn judge calls when the
    data is on disk). Deterministic and identical to the fresh path (same
    compute_record). Caller has already passed sidecar_derivable."""
    rows = load_jsonl(sidecar_path)
    by_id = {r["id"]: r for r in rows}
    records = [compute_record(i, by_id[i]["judge_output"], prompts[i], criteria)
               for i in SUBSET_IDS]
    scored = write_arm_outputs(name, records, rows, out_dir)
    print(f"  ARM {name}: derived from {sidecar_path.name} — 0 judge calls")
    return scored


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--arm", choices=["local", "ab", "hybrid", "both"],
                    default="both")
    ap.add_argument("--landed-root", type=Path, default=None,
                    help="override the article-1 landing root for the "
                         "hybrid arm (e.g. demo13/runs/deep — order t6a)")
    ap.add_argument("--arm-label", default=None,
                    help="output dir + manifest arm name for the hybrid "
                         "arm (e.g. deep) — the pre-registered t6a flight")
    ap.add_argument("--out", type=Path, default=HERE / "flights")
    ap.add_argument("--dry-run", action="store_true",
                    help="validate inputs + build every prompt, NO judge call")
    ap.add_argument("--resume", type=Path, default=None, metavar="FLIGHT_DIR",
                    help="resume a crashed flight: each arm whose persisted "
                         "judge_output.jsonl covers all 10 tasks with our "
                         "judge's output is re-derived from disk (0 judge "
                         "calls); every other arm runs fresh")
    ap.add_argument("--retry", type=int, default=None, metavar="TASK_ID",
                    help="with --resume: ONE fresh judge call for TASK_ID, "
                         "merged with the arm sidecar that lacks it and "
                         "re-derived complete (one decider). Refuses when "
                         "the task is already scored or the sidecar misses "
                         "any other task. No other judge calls.")
    args = ap.parse_args()

    # the t6a extension: a named label re-brands the hybrid arm's
    # article-1 source (pre-registered; the recipe is unchanged)
    flown_hybrid = args.arm_label or "hybrid"
    if args.arm_label and args.arm != "hybrid":
        sys.exit("exit 3: --arm-label requires --arm hybrid")
    if args.landed_root and args.arm != "hybrid":
        sys.exit("exit 3: --landed-root requires --arm hybrid")

    check_clone()
    prompts = frozen_prompts()
    criteria = criteria_by_id()
    references = reference_by_id()
    for i in SUBSET_IDS:
        if i not in criteria or i not in references:
            sys.exit(f"exit 3: criteria/reference row missing for id {i}")

    def load_landed_articles(root: Path) -> dict:
        articles = {}
        for i in SUBSET_IDS:
            rp = landed_report(i, root)
            # charter linkage: the flight answered the frozen prompt (never
            # silent)
            charter = json.loads(
                (rp.parent / "charter.json").read_text(encoding="utf-8"))
            if charter.get("question") != prompts[i]:
                sys.exit(f"exit 3: id {i} charter question != frozen prompt:\n"
                         f"  charter: {charter.get('question')!r}\n"
                         f"  frozen:  {prompts[i]!r}")
            articles[i] = rp.read_text(encoding="utf-8")
        return articles

    local_articles = None
    if args.arm in ("local", "both"):
        local_articles = load_landed_articles(DEMO12_LOCAL)
    hybrid_articles = None
    if args.arm == "hybrid":
        hybrid_articles = load_landed_articles(
            args.landed_root or DEMO12_HYBRID)

    arts = subset_articles()
    for i in SUBSET_IDS:
        if i not in arts:
            sys.exit(f"exit 3: subset-articles row missing for id {i}")
        if arts[i]["prompt"] != prompts[i]:
            sys.exit(f"exit 3: ab id {i} prompt != frozen prompt")
    ab_articles = {i: arts[i]["article"] for i in SUBSET_IDS}

    # per-arm mode, decided and NAMED before any guard: derive-from-disk when
    # the persisted sidecar is complete and ours, else fresh; one arm may be
    # a single-task RETRY (§18.3)
    modes = {}
    retry_arm = None
    if args.resume is not None and not args.resume.is_dir():
        sys.exit(f"exit 3: resume dir not found: {args.resume}")
    if args.retry is not None:
        if args.retry not in SUBSET_IDS:
            sys.exit(f"exit 3: --retry id {args.retry} not in subset {SUBSET_IDS}")
        if args.resume is None:
            sys.exit("exit 3: --retry requires --resume (the arm sidecar to "
                     "merge into)")
        if args.dry_run:
            sys.exit("exit 3: --retry is a real judge call — incompatible "
                     "with --dry-run")
        # the retry targets the arm whose persisted sidecar lacks the id —
        # self-describing, no --arm ambiguity
        lacking = []
        for a in ("local", "ab", flown_hybrid):
            sp = args.resume / a / "judge_output.jsonl"
            if sp.exists():
                ids = {r["id"] for r in load_jsonl(sp)}
                if args.retry not in ids:
                    lacking.append(a)
        if len(lacking) != 1:
            sys.exit(f"exit 3: --retry {args.retry}: arm(s) lacking the id: "
                     f"{lacking}, expected exactly one")
        retry_arm = lacking[0]
    for a in ("local", "ab", flown_hybrid):
        if a == retry_arm:
            modes[a] = "retry"
            continue
        if not args.dry_run and args.resume is not None:
            sp = args.resume / a / "judge_output.jsonl"
            if sp.exists():
                if sidecar_derivable(a, sp):
                    modes[a] = "derive"
                    continue
            else:
                print(f"resume: arm {a} — no persisted sidecar, running fresh")
        modes[a] = "fresh"

    # the judge guard applies only to arms actually selected by --arm
    # (a re-labeled hybrid arm flies under its label — the guard must
    # key on the flown name, never silently skip)
    if args.arm == "both":
        in_scope = ("local", "ab")
    elif args.arm == "hybrid":
        in_scope = (flown_hybrid,)
    else:
        in_scope = (args.arm,)
    client = None
    if not args.dry_run and any(modes[a] in ("fresh", "retry")
                                for a in in_scope):
        models = served_models()
        if JUDGE_PIN not in models or not models[JUDGE_PIN]:
            sys.exit(f"exit 2: judge {JUDGE_PIN} not LOADED (have {models}) — "
                     f"the 122B window is not open; no judge call made")
        print(f"judge guard: {JUDGE_PIN} loaded")
        client = AIClient(model=JUDGE_PIN)
    elif not args.dry_run:
        print("resume: every arm derives from disk — no judge calls, no guard")
    else:
        try:
            print(f"dry-run: judge load truth: {served_models()}")
        except Exception as e:                       # noqa: BLE001
            print(f"dry-run: /v1/models unreachable ({e}) — ignored (no judge "
                  f"call in dry-run)")

    ts = time.strftime("%Y%m%dT%H%M%S")
    out_dir = args.out / f"race-{ts}"
    if args.dry_run:
        out_dir = args.out / "dry-run"
    print(f"output root: {out_dir}")

    scored_counts = {}
    if args.arm in ("local", "both"):
        scored_counts["local"] = run_one_arm(
            "local", local_articles, prompts, criteria, references,
            client, args.dry_run, out_dir, modes, args)
    if args.arm in ("ab", "both"):
        scored_counts["ab"] = run_one_arm(
            "ab", ab_articles, prompts, criteria, references,
            client, args.dry_run, out_dir, modes, args)
    if args.arm == "hybrid":
        scored_counts[flown_hybrid] = run_one_arm(
            flown_hybrid, hybrid_articles, prompts, criteria, references,
            client, args.dry_run, out_dir, modes, args)

    if not args.dry_run:
        manifest = {
            "order": "deep-research-t5a",
            "created_at": ts,
            "resumed_from": str(args.resume) if args.resume else None,
            "retry": ({"arm": retry_arm, "id": args.retry}
                      if args.retry is not None else None),
            "judge": {"pin": JUDGE_PIN, "caveat": "different model from the "
                      "official judges (gemini-2.5-pro / GPT-5.5 era)"},
            "arms": {k: {"tasks": len(SUBSET_IDS), "scored": scored_counts[k],
                         "source": modes[k]} for k in scored_counts},
            "cleaning": "article_1 uncleaned in both arms (named caveat — the "
                        "report IS the deliverable; the official cleaned "
                        "targets are not shipped)",
            "inputs": {"clone_pin": PIN,
                       "subset_articles_sha": SUBSET_ARTICLES_SHA},
            "landed_arm": flown_hybrid if args.arm == "hybrid" else "local",
            "landed_dirs": {str(i): str(landed_report(
                i, (args.landed_root or DEMO12_HYBRID)
                if args.arm == "hybrid" else DEMO12_LOCAL
            ).parent) for i in SUBSET_IDS},
        }
        (out_dir / "manifest.json").write_text(
            json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        print(f"manifest: {out_dir / 'manifest.json'}")

        # zero scored tasks in any arm is a NAMED failure, never green
        # (four-verdicts — same rule as the house test gate: a zero-test run
        # exits 4, never a silent pass)
        zero = [a for a, n in scored_counts.items() if n == 0]
        if zero:
            print(f"FAILED: 0 tasks scored in arm(s) {zero} — not green; "
                  f"failed ids named in each arm's errors.jsonl")
            sys.exit(4)


if __name__ == "__main__":
    main()
