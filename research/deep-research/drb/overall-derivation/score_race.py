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

# Published-peer article sets for the A/B arm. Each is the leaderboard space's
# own `data/raw_data/<system>.jsonl`, subset to SUBSET_IDS, prompt-linked
# against the frozen query.subset.jsonl at extraction. Adding a peer is a row
# here, never a second scorer: the A/B arm has ONE implementation and the peer
# only decides which article occupies article_1 (§10.6, §4).
PEERS = {
    "perplexity": ("perplexity-subset-articles.jsonl", SUBSET_ARTICLES_SHA),
    "aiq": ("aiq-subset-articles.jsonl",
            "3aed2e68ba505e89e77a84249afa8bf49eebdefc47219ac6cb3e871c18479a04"),
}
JUDGE_PIN = "Qwen3.8-27B-UD-Q6_K_XL"   # the daemon primary — the standard
# stack (order deep-research-t7a amendment, directive 7f0e276b,
# pre-registered pre-registration.md "T7a amendment — the DRB-I flight":
# rung-1 baseline on the 27B; the 122B window is rung 2, gated on this
# baseline)
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


# ---- the judge endpoint: ONE decider, resolved BEFORE api.py is imported ----
# `judge_instrument.resolve_judge_endpoint` owns this (see its 2026-08-26
# amendment): the vendored api.py binds LLM_BACKEND and its base URL at IMPORT
# time and defaults to openrouter, so the resolution must happen above that
# import, and the /models guard below must verify the SAME endpoint the judge
# calls reach — otherwise the guard passes while the instrument is substituted.
sys.path.insert(0, str(REPO))
from judge_instrument import configure_judge_client                # noqa: E402

JUDGE_BASE_URL = configure_judge_client()

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

# The judge instrument — the greedy sampling pin (amendment N6) and the
# scorable-verdict predicate — is ONE decider shared with
# arms/lab/score_one.py (§10.6). A pinned reading is never compared
# against an unpinned one, so both scorers must pin identically.
sys.path.insert(0, str(REPO))
from judge_instrument import (DIMS, JUDGE_TEMPERATURE, JUDGE_TOP_P,   # noqa: E402
                             pin_sampling, unscorable)

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
    """The ONE landed report for a task in a demo12 arm root (amendment-2a
    gate: verdict-set.json present). Refuses if zero or multiple landed dirs —
    never guesses. The root names the arm: demo12/runs/{local,hybrid}/.
    Since 2026-08-19 the loop writes render-race.md (the clean article
    page: typed citations, stamped downgrades) beside report.md; the
    scorer prefers it when present and falls back to the transcript
    (old flights) — named, no silent substitution."""
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
    run_dir = landed[0]
    race = run_dir / "render-race.md"
    return race if race.exists() else run_dir / "report.md"


def subset_articles(peer: str = "perplexity") -> dict:
    name, pin = PEERS[peer]
    f = HERE / "inputs" / name
    got = sha256_of(f)
    if got != pin:
        sys.exit(f"exit 3: {peer} subset-articles sha256 {got[:12]}… != "
                 f"pinned {pin[:8]}…")
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
    base = JUDGE_BASE_URL.rstrip("/")
    r = requests.get(f"{base}/models", timeout=30)
    r.raise_for_status()
    return {m.get("id"): bool(m.get("performance", {}).get("loaded"))
            for m in r.json().get("data", [])}


# ---- settling before a judge prefill ---------------------------------------
#
# WHAT THIS IS NOT, and why. The first version of this keyed on the daemon's
# own RSS with a 25 GiB threshold copied from `bed/run-ceiling.sh`, and
# restarted the daemon above it. Both halves were wrong, measured the same
# morning:
#
#   * 39-40 GiB IS the working set of one judge call with the 27B loaded and a
#     ~35k-token prompt prefilled — not accumulation. An RSS threshold below it
#     fires before EVERY task.
#   * `sovereign daemon restart` runs the startup freshness gate, which spawns
#     `rust-analyzer scip .` as a daemon child. That reindex costs ~13 GiB and
#     several minutes, and it would then race the very prefill the restart was
#     supposed to protect. Worse, a SCIP export killed part-way wipes the
#     code-intel graph. The "precaution" was strictly more dangerous than
#     doing nothing.
#
# What actually threatens the run is HOST pressure — the daemon is this host's
# designated OOM victim (notes 05cbffed / f2afc2cf), so what matters is whether
# the kernel is about to start killing, not what the daemon weighs. Co-tenants
# are the usual cause and they are transient, so the right response is to WAIT,
# which has no side effects at all.
# A FIXED FLOOR IS THE WRONG SHAPE, and this is the measurement that says so.
# On 2026-08-26 settle correctly waited out a co-tenant for task 62, admitted
# the prefill at 18.3 GiB available — and the daemon was OOM-killed at 45.3 GiB
# anon-rss four minutes later. The floor was not too low for the host; it was
# too low FOR THAT TASK. A judge prefill's memory scales with its prompt, and
# task 62's is 208,513 chars against task 56's 137,233.
#
# Two-point calibration (§18.5 — a two-point fit is a slope, not a law; a third
# observation re-derives it here):
#   t56 137,233 chars -> daemon peaked ~39.5 GiB
#   t62 208,513 chars -> OOM-killed at 45.3 GiB
#   => ~0.082 GiB per 1k prompt chars, over a ~28.2 GiB model-resident base.
# So a 208k-char task needs ~17.1 GiB of headroom and a 137k-char one ~11.3.
# The old constant 12.0 was right for one of those and fatal for the other.
PREFILL_GIB_PER_1K_CHARS = 0.082
SETTLE_MARGIN_GIB = 6.0        # the rest of the box, and the fit's own slack
SETTLE_FLOOR_GIB = 12.0        # absolute floor, whatever the prompt size
SETTLE_MAX_WAIT_S = 900

# THE WALL IS ~55 GiB, NOT THE BOX'S 125 GB. Read off seven global OOM kills on
# 2026-08-26 (boot -1, `journalctl -b -1 -k`), daemon RSS + rust-analyzer RSS at
# each: 54.5 / 49.1 / 55.4 / 53.7 / 56.0 / 55.7. The Halo's GPU holds the rest
# unreclaimably, so MemAvailable is NOT the quantity that decides whether a
# prefill survives — the sum of the resident tenants against this wall is.
OBSERVED_WALL_GIB = 55.0
MODEL_BASE_GIB = 28.2          # the resident judge model, from the same fit

# `daemon restart` spawns `rust-analyzer scip .` as a CHILD (~12 GiB). In the
# 2026-08-26 crash loop it was resident at every kill from 08:32 on, and it is
# EXACTLY the difference between t62's 45.3 GiB fitting under the wall and not.
# It is never killed here: a half-killed SCIP export wipes the code-intel graph.
# It is waited out.


def prefill_headroom_gib(prompt_chars: int) -> float:
    """Host memory this prefill needs ON TOP of the already-resident model."""
    return max(
        SETTLE_FLOOR_GIB,
        prompt_chars / 1000.0 * PREFILL_GIB_PER_1K_CHARS + SETTLE_MARGIN_GIB,
    )


def mem_available_gib() -> float:
    try:
        with open("/proc/meminfo") as f:
            for line in f:
                if line.startswith("MemAvailable:"):
                    return int(line.split()[1]) / (1024 ** 2)
    except Exception:                                   # noqa: BLE001
        pass
    return float("inf")


def scip_child_gib() -> float:
    """Resident GiB of the daemon's `rust-analyzer scip` child, 0.0 if absent.

    A competitor for the same wall, and the one that turned a survivable t62
    into a seven-kill crash loop. Named so settle() can wait it out.
    """
    total = 0.0
    try:
        pids = subprocess.run(["pgrep", "-f", "rust-analyzer"],
                              capture_output=True, text=True).stdout.split()
        for pid in pids:
            try:
                with open(f"/proc/{pid}/statm") as f:
                    total += int(f.read().split()[1]) * 4096 / (1024 ** 3)
            except OSError:
                continue
    except Exception:                                   # noqa: BLE001
        return 0.0
    return total


def daemon_pid() -> str | None:
    """PID of the serving daemon, or None. A CHANGED pid mid-arm means the
    daemon was OOM-killed and systemd restarted it — the crash loop."""
    out = subprocess.run(["pgrep", "-f", "sovereign-cli-daemon daemon run"],
                         capture_output=True, text=True).stdout.split()
    return out[0] if out else None


def daemon_rss_gib() -> float:
    """Resident GiB of the serving daemon, or 0.0 if it cannot be read."""
    try:
        out = subprocess.run(["pgrep", "-f", "sovereign-cli-daemon daemon run"],
                             capture_output=True, text=True).stdout.split()
        if not out:
            return 0.0
        with open(f"/proc/{out[0]}/statm") as f:
            return int(f.read().split()[1]) * 4096 / (1024 ** 3)
    except Exception:                                   # noqa: BLE001
        return 0.0


def judge_ready(base: str, timeout_s: int = 900) -> bool:
    """A READINESS probe, not a liveness one (§9.5). `/v1/models` answered 200
    while the daemon still could not serve a completion and it cost a cell on
    2026-08-26; this asks for one token from the pinned judge and only returns
    True when that token arrives."""
    import requests
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            r = requests.post(
                f"{base.rstrip('/')}/chat/completions",
                json={"model": JUDGE_PIN, "max_tokens": 1,
                      "messages": [{"role": "user", "content": "ok"}]},
                timeout=180,
            )
            if r.status_code == 200:
                return True
        except Exception:                               # noqa: BLE001
            pass
        time.sleep(10)
    return False


class OverTheWall(RuntimeError):
    """This prefill cannot fit under the observed wall. Named, never attempted:
    attempting it OOM-kills the daemon, systemd restarts it, the restart
    respawns a 12 GiB SCIP child, and the next task inherits a worse host than
    this one did. That is the 2026-08-26 crash loop, and it cost the machine."""


def projected_peak_gib(prompt_chars: int) -> tuple[float, float, float]:
    """(projected_peak, prefill_need, competitor_rss) for THIS prompt.

    The quantity that decides survival is the sum of resident tenants against
    OBSERVED_WALL_GIB — NOT MemAvailable, which spikes immediately after an OOM
    kill (the 45 GiB daemon just died) and so waves the next attempt through at
    the single worst moment, into a host about to reload the model AND respawn
    the SCIP child.
    """
    prefill = prompt_chars / 1000.0 * PREFILL_GIB_PER_1K_CHARS
    competitors = scip_child_gib()
    resident = daemon_rss_gib()
    # THE ARENA IS REUSED, NOT ACCUMULATED — and the first cut of this guard
    # got that wrong in a way worth keeping written down. It computed
    # `max(MODEL_BASE, resident) + prefill`, which ADDS the next prefill on top
    # of a daemon that is still holding the last one. Measured 2026-08-26: the
    # daemon settles to 45.6 GiB between tasks and does not fall back to its
    # ~6 GiB idle, so that form refused t65 (a SMALLER prompt than the t62 that
    # had just succeeded) at a projected 66.8 GiB on a host with room to spare.
    # If prefills were additive, t62 after t56 would have peaked at
    # 39.5 + 17.1 = 56.6 GiB; it peaked at 45.3. The arena grows to fit the
    # LARGEST prompt seen and is then re-used, so the projection is a MAX, not
    # a sum — and it still tracks real growth, because a resident figure that
    # creeps upward raises the projection with it.
    want = MODEL_BASE_GIB + prefill
    base = max(resident, want)
    return (base + competitors + SETTLE_MARGIN_GIB, prefill, competitors)


def settle(task_id: int, prompt_chars: int) -> None:
    """Wait until the host can hold THIS prefill, then admit it — or REFUSE.

    Never restarts anything and never kills the SCIP child (a half-killed scip
    export wipes the code-intel graph). It waits the competitor out, because
    the competitor finishes on its own.

    Refusal is not a silent substitution (§18.3): the task becomes a NAMED
    error record, the sidecar is already on disk, and `--resume` picks it up on
    a host that can hold it. Proceeding into a predicted kill is the thing that
    reports nothing and destroys the run.
    """
    peak, prefill, competitors = projected_peak_gib(prompt_chars)
    if peak <= OBSERVED_WALL_GIB:
        return
    print(f"  id {task_id}: {prompt_chars:,}-char prompt projects to "
          f"{peak:.1f}G = max(resident {daemon_rss_gib():.1f}, "
          f"base {MODEL_BASE_GIB:.1f} + prefill {prefill:.1f}) "
          f"+ competitors {competitors:.1f} + margin {SETTLE_MARGIN_GIB:.1f} "
          f"against a {OBSERVED_WALL_GIB:.0f}G wall — waiting", flush=True)
    deadline = time.time() + SETTLE_MAX_WAIT_S
    while time.time() < deadline:
        time.sleep(15)
        peak, prefill, competitors = projected_peak_gib(prompt_chars)
        if peak <= OBSERVED_WALL_GIB:
            print(f"  id {task_id}: projects to {peak:.1f}G, competitors now "
                  f"{competitors:.1f}G — proceeding", flush=True)
            return
    raise OverTheWall(
        f"projects to {peak:.1f}G against a {OBSERVED_WALL_GIB:.0f}G wall after "
        f"{SETTLE_MAX_WAIT_S}s (prefill {prefill:.1f}G, competitors "
        f"{competitors:.1f}G) — REFUSED, not attempted. Re-run with --resume "
        f"once the competitors are gone.")


def judge_call(client: AIClient, prompt: str, task_id: int) -> dict:
    """One task's judge call with the official retry recipe (10 × 1.5^retry
    backoff), paced to the daemon's queue-shed signal: when the 503 names a
    retry_after_secs (local_queue_full), the backoff honors it —
    sleep(max(1.5^(retry+1), retry_after_secs)) — so a retry lands in the
    slot's idle gap instead of firing inside the busy window. Judge prompt,
    output and scoring math are untouched (transport resilience only).
    Returns the parsed, dim-complete JSON."""
    last_err = None
    for retry in range(MAX_RETRIES):
        try:
            raw = client.generate(user_prompt=prompt, system_prompt="")
            extracted = extract_json_from_markdown(raw)
            if not extracted:
                raise ValueError("no JSON extracted from judge response")
            out = json.loads(extracted)
            why = unscorable(out)
            if why:
                raise ValueError(f"unscorable judge verdict: {why}")
            return out
        except Exception as e:                      # noqa: BLE001 — official recipe
            last_err = e
            if retry + 1 < MAX_RETRIES:
                m = re.search(r'retry_after_secs["\s:=]+(\d+)', str(e))
                shed = int(m.group(1)) if m else 0
                time.sleep(max(1.5 ** (retry + 1), shed))
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

    # Sized against the prompt that is about to be sent, not against a guess
    # made before it was built — which is why this lives here and not in the
    # caller's loop.
    settle(task_id, len(prompt))
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


def seed_rows(name: str, sidecar_path: Path) -> list:
    """Rows of a PARTIAL persisted sidecar that this run may reuse instead of
    re-judging.

    `--resume` used to be all-or-nothing: a sidecar covering all ten tasks was
    derived with zero judge calls, and one covering nine was thrown away and
    the whole arm re-judged. Now that the sidecar is flushed after every task,
    the partial case is the COMMON case — it is exactly what a crash, an OOM
    kill or an interrupted arm leaves behind — and re-burning eight good judge
    calls to recover the ninth is the waste this avoids.

    The same identity guard applies as to a full derive: rows that are not our
    judge's complete output are refused, and the refusal is NAMED (§18.3).
    Returns [] when there is nothing safely reusable.
    """
    if not sidecar_path.exists():
        return []
    rows = load_jsonl(sidecar_path)
    issues = _sidecar_issues(name, rows)
    if issues:
        print(f"  ARM {name}: seed sidecar REFUSED — " + "; ".join(issues)
              + " — every task runs fresh")
        return []
    ids = sorted(r["id"] for r in rows)
    print(f"  ARM {name}: seeding {len(ids)} task(s) from {sidecar_path} "
          f"— ids {ids} will NOT be re-judged")
    return rows


def run_arm(name: str, article_1s: dict, prompts: dict, criteria: dict,
            references: dict, client: AIClient | None, dry_run: bool,
            out_dir: Path, skipped: frozenset = frozenset(),
            seed: list | None = None) -> int:
    """One arm, fresh judge calls. Returns the number of scored tasks.
    Skipped ids (never-ran) print a named NEVER-RAN line — never a judge
    call, never an error row. Seeded ids are reused from a prior sidecar and
    are likewise never a judge call; both are named."""
    print(f"== arm {name} (fresh judge calls) ==")
    records, sidecar = [], []
    seeded = {r["id"]: r for r in (seed or [])}
    for tid in sorted(seeded):
        if tid in skipped:
            continue
        sidecar.append(seeded[tid])
        records.append(compute_record(tid, seeded[tid]["judge_output"],
                                      prompts[tid], criteria))
        print(f"  id {tid}: REUSED from the seed sidecar — no judge call "
              f"(overall {records[-1]['overall_score']*100:.2f})", flush=True)
    # The sidecar is flushed after EVERY task, not at the end of the arm.
    # `--resume` documents the persisted judge_output.jsonl as the evidence a
    # crash is recovered from — but it was only written by write_arm_outputs
    # once all ten tasks had landed, so the very crash it exists to survive
    # destroyed its own input. On a host whose daemon is the designated OOM
    # victim, a 90-minute arm losing nine completed judge calls to the tenth
    # is not a hypothetical.
    # THE SEEDS REACH DISK BEFORE THE FIRST FRESH JUDGE CALL. Until 2026-08-26
    # the flush lived only at the FOOT of the task loop, so a resume that died
    # inside its first fresh task wrote nothing at all: the three seeded rows
    # sat in memory and went down with the process. That is exactly what
    # race-20260826T084028 is — an empty arm dir after a resume of three tasks.
    # The rows survived only because the dir they were seeded FROM still
    # existed. Durability that depends on the previous run's luck is not
    # durability.
    live = None
    if not dry_run:
        live = out_dir / name / "judge_output.jsonl"
        live.parent.mkdir(parents=True, exist_ok=True)
        flush_sidecar(live, sidecar)
    watched_pid = daemon_pid()
    for task_id in SUBSET_IDS:
        if task_id in skipped:
            print(f"  id {task_id}: NEVER-RAN (pre-registered cap stop) — "
                  f"skipped, no judge call")
            continue
        if task_id in seeded:
            continue
        try:
            rec = score_task(task_id, prompts, criteria, references,
                             article_1s[task_id], client, dry_run, sidecar)
            if rec is not None:
                records.append(rec)
                print(f"  id {task_id}: overall {rec['overall_score']*100:.2f}",
                      flush=True)
        except Exception as e:                       # noqa: BLE001
            # the official driver's own behavior: a failed task is an ERROR
            # record, the run continues — a transient daemon blip must not
            # abort the flight (§18.2: the failure is named, never silent)
            records.append({"id": task_id, "prompt": prompts[task_id],
                            "error": str(e)})
            print(f"  id {task_id}: ERROR — {e} (recorded, flight continues)")
        if live is not None:
            flush_sidecar(live, sidecar)
        now_pid = daemon_pid()
        if watched_pid is not None and now_pid != watched_pid:
            # The daemon died and systemd restarted it. Continuing feeds the
            # crash loop: every restart respawns a ~12 GiB SCIP child, so the
            # NEXT task faces a worse host than this one did. Seven of these
            # took the machine down on 2026-08-26. Stop; the sidecar is on
            # disk; --resume continues on a host that can hold the work.
            print(f"  DAEMON RESTARTED mid-arm (pid {watched_pid} -> "
                  f"{now_pid}) — it was OOM-killed. STOPPING this arm rather "
                  f"than feeding the crash loop. {len(sidecar)} task(s) are "
                  f"flushed to {live}; resume from there.", flush=True)
            break
    if dry_run:
        return 0
    order = {t: i for i, t in enumerate(SUBSET_IDS)}
    records.sort(key=lambda r: order.get(r["id"], len(order)))
    sidecar.sort(key=lambda r: order.get(r["id"], len(order)))
    return write_arm_outputs(name, records, sidecar, out_dir)


def flush_sidecar(live, sidecar: list) -> None:
    """Persist the arm sidecar. ONE implementation, called after seeding and
    after every task — the only writer of the file `--resume` reads."""
    with open(live, "w", encoding="utf-8") as f:
        for row in sidecar:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")


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


def sidecar_derivable(name: str, sidecar_path: Path,
                      skipped: frozenset = frozenset()) -> bool:
    """True when the persisted sidecar covers every non-skipped task with
    OUR judge's complete output. Refusal reasons are NAMED here, never
    silent (§18.3)."""
    rows = load_jsonl(sidecar_path)
    issues = _sidecar_issues(name, rows)
    by_id = {r["id"]: r for r in rows}
    missing = [i for i in SUBSET_IDS if i not in by_id and i not in skipped]
    if missing:
        issues.append(f"missing ids {missing}")
    if issues:
        # NOT "running fresh": a partial-but-ours sidecar is SEEDED by
        # run_arm, which then judges only the missing ids and names both.
        # Saying "fresh" here would misreport a run that reused eight of ten
        # judge calls — the message has to match what the code does.
        print(f"  ARM {name}: persisted sidecar not fully derivable — "
              + "; ".join(issues))
        return False
    return True


def retry_sidecar_ready(name: str, sidecar_path: Path, retry_id: int,
                        skipped: frozenset = frozenset()) -> bool:
    """True when the persisted sidecar covers exactly the other non-skipped
    tasks with OUR judge's output — the precondition for a one-call retry.
    Refusal reasons are NAMED here, never silent (§18.3)."""
    rows = load_jsonl(sidecar_path)
    issues = _sidecar_issues(name, rows)
    by_id = {r["id"]: r for r in rows}
    if retry_id in by_id:
        issues.append(f"task {retry_id} already scored — nothing to retry")
    missing = [i for i in SUBSET_IDS if i not in by_id and i not in skipped]
    if missing != [retry_id]:
        issues.append(f"coverage mismatch: missing {missing}, expected "
                      f"exactly [{retry_id}]")
    if issues:
        print(f"  ARM {name}: retry refused — " + "; ".join(issues))
        return False
    return True


def retry_task(name: str, retry_id: int, sidecar_path: Path, article_1s: dict,
               prompts: dict, criteria: dict, references: dict,
               client: AIClient, out_dir: Path,
               skipped: frozenset = frozenset()) -> int:
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
               for i in SUBSET_IDS if i not in skipped]
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
    skipped = args.skip
    if modes[a] == "derive":
        return derive_from_sidecar(a, args.resume / a / "judge_output.jsonl",
                                   prompts, criteria, out_dir, skipped)
    if modes[a] == "retry":
        sp = args.resume / a / "judge_output.jsonl"
        if not retry_sidecar_ready(a, sp, args.retry, skipped):
            sys.exit(f"exit 3: retry {args.retry} refused in arm {a} — "
                     f"see above")
        return retry_task(a, args.retry, sp, article_1s, prompts, criteria,
                          references, client, out_dir, skipped)
    seed = []
    if args.resume is not None and not dry_run:
        sp = args.resume / a / "judge_output.jsonl"
        if sp.exists():
            seed = seed_rows(a, sp)
    return run_arm(a, article_1s, prompts, criteria, references,
                   client, dry_run, out_dir, skipped, seed)


def derive_from_sidecar(name: str, sidecar_path: Path, prompts: dict,
                        criteria: dict, out_dir: Path,
                        skipped: frozenset = frozenset()) -> int:
    """Re-derive an arm from a persisted judge_output.jsonl — ZERO judge
    calls (the seat's recovery order: never re-burn judge calls when the
    data is on disk). Deterministic and identical to the fresh path (same
    compute_record). Caller has already passed sidecar_derivable."""
    rows = load_jsonl(sidecar_path)
    by_id = {r["id"]: r for r in rows}
    records = [compute_record(i, by_id[i]["judge_output"], prompts[i], criteria)
               for i in SUBSET_IDS if i not in skipped]
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
    ap.add_argument("--peer", choices=sorted(PEERS), default="perplexity",
                    help="which published system's own articles occupy "
                         "article_1 in the A/B arm (default perplexity — the "
                         "2026-08-17 judge-offset arm). Each peer's input is "
                         "sha256-pinned in PEERS; the recipe is identical.")
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
    ap.add_argument("--skip-tasks", default="", metavar="IDS",
                    help="comma-separated subset ids to SKIP (never-ran "
                         "tasks — the pre-registered cumulative-search cap "
                         "stop). Named in the manifest with the reason; "
                         "never a judge call, never an error row; means "
                         "stay over the SCORED rows only (unchanged "
                         "aggregation).")
    args = ap.parse_args()

    try:
        skipped = frozenset(int(x) for x in args.skip_tasks.split(",") if x)
    except ValueError:
        sys.exit(f"exit 3: --skip-tasks ids must be integers: "
                 f"{args.skip_tasks!r}")
    bad = sorted(skipped - set(SUBSET_IDS))
    if bad:
        sys.exit(f"exit 3: --skip-tasks ids {bad} not in subset {SUBSET_IDS}")
    if skipped:
        print(f"skip: {sorted(skipped)} — never-ran (pre-registered cap "
              f"stop); named in the manifest, excluded from the means over "
              f"scored rows only")
    args.skip = skipped

    # the t6a extension: a named label re-brands the hybrid arm's
    # article-1 source (pre-registered; the recipe is unchanged)
    flown_hybrid = args.arm_label if args.arm == "hybrid" and args.arm_label \
        else "hybrid"
    if args.arm_label and args.arm not in ("hybrid", "ab"):
        sys.exit("exit 3: --arm-label requires --arm hybrid or --arm ab")
    # a non-default peer MUST be named in the arm label, so an artifact tree
    # can never leave "which system wrote article_1" to be inferred (§18.3)
    flown_ab = args.arm_label if args.arm == "ab" and args.arm_label else "ab"
    if args.peer != "perplexity" and flown_ab == "ab":
        sys.exit(f"exit 3: --peer {args.peer} requires --arm-label "
                 f"(e.g. --arm-label ab-{args.peer}) so the arm dir names "
                 f"whose articles were scored")
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
            if i in skipped:
                continue
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

    arts = subset_articles(args.peer)
    for i in SUBSET_IDS:
        if i not in arts:
            sys.exit(f"exit 3: {args.peer} subset-articles row missing "
                     f"for id {i}")
        if arts[i]["prompt"] != prompts[i]:
            sys.exit(f"exit 3: ab({args.peer}) id {i} prompt != frozen prompt")
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
        for a in ("local", flown_ab, flown_hybrid):
            sp = args.resume / a / "judge_output.jsonl"
            if sp.exists():
                ids = {r["id"] for r in load_jsonl(sp)}
                if args.retry not in ids:
                    lacking.append(a)
        if len(lacking) != 1:
            sys.exit(f"exit 3: --retry {args.retry}: arm(s) lacking the id: "
                     f"{lacking}, expected exactly one")
        retry_arm = lacking[0]
    for a in ("local", flown_ab, flown_hybrid):
        if a == retry_arm:
            modes[a] = "retry"
            continue
        if not args.dry_run and args.resume is not None:
            sp = args.resume / a / "judge_output.jsonl"
            if sp.exists():
                if sidecar_derivable(a, sp, skipped):
                    modes[a] = "derive"
                    continue
                # partial-but-ours is not "no sidecar": run_arm seeds from it
                # and judges only what is missing (seed_rows names what it
                # reused, or why it refused).
            else:
                print(f"resume: arm {a} — no persisted sidecar, running fresh")
        modes[a] = "fresh"

    # the judge guard applies only to arms actually selected by --arm
    # (a re-labeled hybrid arm flies under its label — the guard must
    # key on the flown name, never silently skip)
    if args.arm == "both":
        in_scope = ("local", flown_ab)
    elif args.arm == "hybrid":
        in_scope = (flown_hybrid,)
    elif args.arm == "ab":
        in_scope = (flown_ab,)
    else:
        in_scope = (args.arm,)
    client = None
    if not args.dry_run and any(modes[a] in ("fresh", "retry")
                                for a in in_scope):
        models = served_models()
        if JUDGE_PIN not in models or not models[JUDGE_PIN]:
            sys.exit(f"exit 2: judge {JUDGE_PIN} not LOADED (have {models}) — "
                     f"the judge window is not open; no judge call made")
        print(f"judge guard: {JUDGE_PIN} loaded")
        client = pin_sampling(AIClient(model=JUDGE_PIN))
        print(f"judge pin: temperature={JUDGE_TEMPERATURE} top_p={JUDGE_TOP_P} (amendment N6)")
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
        scored_counts[flown_ab] = run_one_arm(
            flown_ab, ab_articles, prompts, criteria, references,
            client, args.dry_run, out_dir, modes, args)
    if args.arm == "hybrid":
        scored_counts[flown_hybrid] = run_one_arm(
            flown_hybrid, hybrid_articles, prompts, criteria, references,
            client, args.dry_run, out_dir, modes, args)

    if not args.dry_run:
        manifest = {
            "order": "deep-research-t7a",
            "created_at": ts,
            "resumed_from": str(args.resume) if args.resume else None,
            "retry": ({"arm": retry_arm, "id": args.retry}
                      if args.retry is not None else None),
            "judge": {"pin": JUDGE_PIN,
                      "sampling": {
                          "temperature": JUDGE_TEMPERATURE,
                          "top_p": JUDGE_TOP_P,
                          "amendment": "greedy — N6, 2026-08-23",
                          # Which tasks this run's pin actually covers. A
                          # resumed arm is re-derived from a sidecar this run
                          # did not produce, and a retry pins exactly ONE
                          # task — stamping either "greedy" wholesale would
                          # substitute a claim for a measurement (§18.3).
                          "covers": {k: ("every task" if modes[k] == "fresh"
                                         else f"task {args.retry} only"
                                         if modes[k] == "retry"
                                         else "no task — re-derived from a "
                                              "sidecar this run did not judge")
                                     for k in scored_counts},
                          "note": "a manifest with NO sampling block was "
                                  "flown before this amendment, at the daemon "
                                  "default temperature 0.7. A pinned reading "
                                  "is never compared against an unpinned one.",
                      },
                      "caveat": "different model from the "
                      "official judges (gemini-2.5-pro / GPT-5.5 era)"},
            "arms": {k: {"tasks": len(SUBSET_IDS) - len(skipped),
                         "scored": scored_counts[k],
                         "source": modes[k]} for k in scored_counts},
            "cleaning": "article_1 uncleaned in both arms (named caveat — the "
                        "report IS the deliverable; the official cleaned "
                        "targets are not shipped)",
            "inputs": {"clone_pin": PIN,
                       "ab_peer": args.peer,
                       "ab_articles_file": PEERS[args.peer][0],
                       "subset_articles_sha": PEERS[args.peer][1]},
            "landed_arm": flown_hybrid if args.arm == "hybrid" else "local",
            "skipped": ({"ids": sorted(skipped),
                         "reason": "never-ran — the pre-registered "
                                   "cumulative-search cap stop (task "
                                   "boundary; per-task allowance never "
                                   "reduced)"} if skipped else None),
            "landed_dirs": {str(i): str(landed_report(
                i, (args.landed_root or DEMO12_HYBRID)
                if args.arm == "hybrid" else DEMO12_LOCAL
            ).parent) for i in SUBSET_IDS if i not in skipped},
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
