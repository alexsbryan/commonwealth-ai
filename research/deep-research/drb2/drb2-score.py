#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""DRB-II scorer (order deep-research-t7a).

The instrument: the OFFICIAL DRB-II evaluation protocol, vendored byte-exact
from the pinned clone (vendor/), executed against the local daemon. One
implementation per threshold/schema (ARCH_PRINCIPLES §10.6): the vendored
PROMPT_TEMPLATE, parse/validate, and aggregation are imported, never
re-implemented. Pre-registration: research/deep-research/adversarial/
pre-registration.md, section "T7a ... DECLARATION" (2026-08-19).

Pre-registered named deviations (all constants, all visible):
  - MAX_PAPER_CHARS  = 45000  (official 150000; our judge's measured context)
  - CHUNK_SIZE       = 4      (amendment N4: shared daemon's MTP inference
                               deadline is 300s — SOVEREIGN_INFERENCE_TIMEOUT_SECS
                               default, model_slot.rs:758 — and 10-item batches
                               straddle it (measured 252.6s and a 300s kill);
                               4-item batches measured 74.6-146.8s, inside with
                               margin. Per-item scoring unchanged: the prompt,
                               validation, aggregation, and retry semantics are
                               untouched; chunk size is transport only)
  - MAX_RETRIES      = 5      (official default)
  - OUTPUT_TOKEN_BUDGET = 16384 (official 32768)
  - reasoning_effort: vendored default "medium"; on HTTP 400/422 the client
    strips the field and retries once, recording the event (deviation #5).

Verdict rules (pre-registered §6): Leg A = paired cluster bootstrap on the
per-task TotalScore delta (ours - Perplexity, same judge, same 8 tasks);
met if CI_lo > 0, failed if CI_hi <= 0, could-not-judge otherwise,
never-ran if no scored flight. Leg B = descriptive reference lines with
caveats, never a gate. Leg C = the -1 channel (blocked_rate), reported.

Calibration (pre-registered §5): M1 same-judge cross-model gap/ordering vs
official (Perplexity 38.58 vs Qwen3-Max 39.25, official gap +0.67);
M2 Presentation band [60, 100]; M3 mechanical channels (blocked fidelity,
evidence fidelity, repeat self-consistency).

DRB2_JUDGE=mock replaces the daemon call with a scripted judge (selftest
only; never a scored run). Selftest: python3 drb2-score.py --selftest
(no daemon, no network).
"""

import argparse
import hashlib
import json
import os
import random
import re
import sys
import time
from pathlib import Path

import requests

sys.path.insert(0, str(Path(__file__).parent / "vendor"))

from prompt_template import PROMPT_TEMPLATE  # noqa: E402
from parse_validate import (  # noqa: E402
    FENCED_JSON_PATTERN,
    parse_model_text,
    validate_batch_result,
)
from aggregation import compute_dimension_averages  # noqa: E402

# --------------------------------------------------------------------------
# Pre-registered constants (the declaration's pinned values)
# --------------------------------------------------------------------------
JUDGE_MODEL = os.environ.get("DRB2_JUDGE_MODEL", "Qwen3.8-27B-UD-Q6_K_XL")
DAEMON_URL = os.environ.get("DRB2_DAEMON_URL", "http://127.0.0.1:9741/v1/chat/completions")
MAX_PAPER_CHARS = int(os.environ.get("DRB2_MAX_PAPER_CHARS", "45000"))
CHUNK_SIZE = int(os.environ.get("DRB2_CHUNK_SIZE", "4"))  # amendment N4, see header
MAX_RETRIES = int(os.environ.get("DRB2_MAX_RETRIES", "5"))
OUTPUT_TOKEN_BUDGET = int(os.environ.get("DRB2_MAX_OUTPUT_TOKENS", "16384"))
PROMPT_TOKEN_BUDGET = int(os.environ.get("DRB2_PROMPT_TOKEN_BUDGET", "16000"))
BOOTSTRAP_SEED_STRING = os.environ.get(
    "DRB2_BOOTSTRAP_SEED_STRING", "deep-research-t7a-bootstrap-2026-08-19"
)
BOOTSTRAP_SEED = int(hashlib.sha256(BOOTSTRAP_SEED_STRING.encode()).hexdigest()[:8], 16)
BOOTSTRAP_N = int(os.environ.get("DRB2_BOOTSTRAP_N", "10000"))

# Official reference lines (t6g teardown leaderboard facts, GPT-5.5-judged,
# 132 tasks; en-only Perplexity 38.03 from paper Table 6). Reference ONLY
# for M1/M2 and Leg B — never a gate input.
OFFICIAL_PERPLEXITY = {"total": 38.58, "ir": 33.05, "analysis": 44.47, "presentation": 79.34}
OFFICIAL_QWEN = {"total": 39.25, "ir": 34.18, "analysis": 48.04, "presentation": 74.59}
OFFICIAL_PERPLEXITY_EN = 38.03
OFFICIAL_GAP = OFFICIAL_QWEN["total"] - OFFICIAL_PERPLEXITY["total"]  # +0.67
M1_GAP_TOLERANCE = 5.0
M2_BAND = (60.0, 100.0)
M3_BLOCKED_ERRORS_ALLOWED = 2
M3_EVIDENCE_FIDELITY_FLOOR = 0.90
M3_REPEAT_AGREEMENT_FLOOR = 0.85
REPEAT_SELF_CONSISTENCY_N = 20

# The loop's shipped report model names in this order (fixed comparison set).
MODELS = ["Perplexity-Research", "Qwen-3-Max-DeepResearch", "ours"]


class Judge:
    """The rubric judge: vendored client schema -> daemon :9741.

    DRB2_JUDGE=mock replaces the LLM call with a scripted rubric judge
    (selftest only). The scripted judge reads the rubric JSON from the
    prompt's <task_and_rubric> block and scores each item:
      - item containing 'known-true'  -> score 1, evidence 'evidence-true'
      - item containing 'known-absent' -> score 0
      - item containing 'known-blocked' -> score -1, evidence quoting the
        blocked title (exercises the M3 blocked channel)
      - prompt containing 'known-parse-error' -> non-JSON first call, valid
        JSON on retry (exercises the vendored retry path)
    The mock emits PRETTY-PRINTED JSON, matching the format the official
    pipeline's judge emits (see parse_fallback below for why).
    """

    def __init__(self):
        self.mock = os.environ.get("DRB2_JUDGE") == "mock"
        self.reasoning_effort = os.environ.get("DRB2_REASONING_EFFORT", "medium")
        self.strip_reasoning_effort = False
        self.strip_events = []
        self.mocked_calls = 0
        self.last_usage = {}
        if not self.mock:
            self._session = requests.Session()

    # -- mock --------------------------------------------------------------
    def _mock(self, prompt: str):
        self.mocked_calls += 1
        if "known-parse-error" in prompt and self.mocked_calls % 2 == 1:
            return "this is not json"
        m = re.search(r"<task_and_rubric>\n(.*)\n</task_and_rubric>", prompt, re.S)
        rubric = json.loads(m.group(1))
        blocked = rubric.get("blocked", {}) or {}
        blocked_title = blocked.get("title", "BLOCKED-TITLE")
        out = []
        for item in rubric.get("rubric_items", []):
            if "known-true" in item:
                out.append({"rubric_item": item, "score": 1,
                            "reason": "mock", "evidence": "evidence-true"})
            elif "known-absent" in item:
                out.append({"rubric_item": item, "score": 0,
                            "reason": "mock", "evidence": ""})
            elif "known-blocked" in item:
                out.append({"rubric_item": item, "score": -1,
                            "reason": "mock", "evidence": f'citing "{blocked_title}"'})
            else:
                out.append({"rubric_item": item, "score": 0,
                            "reason": "mock", "evidence": ""})
        return json.dumps({"results": out}, indent=2)

    # -- real --------------------------------------------------------------
    def _call(self, prompt: str) -> str:
        payload = {
            "model": JUDGE_MODEL,
            "messages": [{"role": "user", "content": prompt}],
            "max_completion_tokens": OUTPUT_TOKEN_BUDGET,
            "stream": False,
        }
        if not self.strip_reasoning_effort and self.reasoning_effort:
            payload["reasoning_effort"] = self.reasoning_effort
        # amendment N3 (pre-registered): DRB2_CLIENT_TIMEOUT default 2700s
        # (45 min) — the vendored 600s default is below the measured batch
        # duration; transport bound only.
        timeout = int(os.environ.get("DRB2_CLIENT_TIMEOUT", "2700"))
        r = self._session.post(DAEMON_URL, json=payload, timeout=timeout)
        if r.status_code >= 400:
            # Deviation #5 (pre-registered): if the daemon rejects the
            # reasoning_effort field, strip it and retry once, recording the
            # event. Glassbox — the calibration record names whether this
            # fired.
            if (not self.strip_reasoning_effort and self.reasoning_effort
                    and r.status_code in (400, 422)):
                print(f"[warn] HTTP {r.status_code} (likely reasoning_effort); "
                      f"retrying without the field: {r.text[:200]}")
                self.strip_reasoning_effort = True
                self.strip_events.append({"ts": time.time(), "status": r.status_code,
                                          "body": r.text[:500]})
                return self._call(prompt)
            print(f"[err] HTTP {r.status_code} body: {r.text[:1000]}")
            r.raise_for_status()
        data = r.json()
        # additive telemetry: last response usage (rate measurement)
        self.last_usage = data.get("usage") or {}
        try:
            content = data["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError) as exc:
            raise ValueError(f"Invalid Chat Completions response: {json.dumps(data)[:1000]}") from exc
        if isinstance(content, list):
            content = "".join(p.get("text", "") for p in content if isinstance(p, dict))
        return content or ""

    def call(self, prompt: str) -> str:
        if self.mock:
            return self._mock(prompt)
        return self._call(prompt)


# --------------------------------------------------------------------------
# The official evaluation protocol (vendored semantics, executed)
# --------------------------------------------------------------------------
def load_tasks(path: str) -> dict:
    """Official load_tasks_data semantics: idx -> content dict."""
    tasks = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            obj = json.loads(line)
            idx = obj.get("idx")
            if idx is None:
                continue
            try:
                key = int(idx)
            except (TypeError, ValueError):
                continue
            tasks[key] = obj.get("content", {})
    return tasks


def read_report_text(path: Path) -> str:
    """Official document extraction semantics: .md read as text (images in
    .docx ignored; we score .md reports only — our loop ships .md, and the
    two fixture models' shipped reports are .md)."""
    if str(path).lower().endswith(".md"):
        with open(path, "r", encoding="utf-8", errors="ignore") as rf:
            return rf.read()
    raise ValueError(f"unsupported file type: {path} (expected .md)")


PARSE_FALLBACK_COUNT = {"count": 0}


def parse_fallback(raw: str):
    """NAMED AMENDMENT N1 (pre-registration, T7a section, 2026-08-19):
    the official _try_clean_and_load corrupts compact single-line JSON
    (measured on the byte-exact vendored copy: pretty JSON parses,
    compact fails at the first '", "key":' adjacency — the official
    pipeline never sees it because its judge emits multi-line JSON).
    Primary path stays the vendored parser, verbatim. If it fails, this
    fallback tries plain json.loads (fenced block first, then raw text)
    and counts every use. The prompt, validation, and aggregation are
    untouched. The count lands in the run's instrument report."""
    fenced = re.findall(FENCED_JSON_PATTERN, raw, re.DOTALL)
    for candidate in [fenced[0] if fenced else None, raw]:
        if not candidate:
            continue
        try:
            return json.loads(candidate.strip()), True
        except json.JSONDecodeError:
            continue
    return None, False


# --------------------------------------------------------------------------
# Amendment N5 (pre-registered 2026-08-20, pre-registration.md T7a section):
# typography-normalized validation + parse robustness. ONE normalization
# function; the vendored validate_batch_result stays the SINGLE validation
# decider (§10.6) — N5 only normalizes both inputs before it runs.
# --------------------------------------------------------------------------
THINK_BLOCK_RE = re.compile(r"<think>.*?</think>", re.DOTALL)
MARKDOWN_DECORATION_RE = re.compile(r"\*\*|~~|`|\*|_")
# N5 last-fence splitter: NON-GREEDY — the vendored FENCED_JSON_PATTERN's
# greedy (.*) spans from the first ```json to the LAST ```, collapsing
# multiple blocks into one capture (measured in selftest). This pattern
# splits blocks individually; the vendored pattern stays byte-exact.
LAST_FENCE_PATTERN = r"```json\s*(.*?)```"
QUOTE_TRANS = str.maketrans({
    "'": "'", '"': "'",
    "‘": "'", "’": "'", "“": "'", "”": "'",
})
THINK_STRIP_COUNT = {"count": 0}
LAST_FENCE_COUNT = {"count": 0}
PARSE_STAGE = {"last": "never"}


def normalize_typography(s: str) -> str:
    """The ONE normalization (N5): casefold; every quote char -> '; strip
    markdown inline decorations (** * _ ` ~~); remove ALL whitespace.
    Applied to BOTH sides of the comparison. Letter-level claim
    substitution is NOT typography and must still fail."""
    s = s.casefold()
    s = s.translate(QUOTE_TRANS)
    s = MARKDOWN_DECORATION_RE.sub("", s)
    return "".join(s.split())


def parse_amended(raw: str):
    """N5 parse path (replaces the direct parse_model_text/fallback chain):
    1) strip <think>...</think> blocks (the 122B judge's CoT prefix broke
       the vendored parser — first-fence latch caught an inner fence,
       full-text parse caught the prefix; the payload after </think> was
       valid JSON), 2) vendored parse_model_text verbatim (fenced-first
       then full text), 3) last-fence attempt (json.loads on the LAST
       ```json fence), 4) N1 parse_fallback (unchanged, counted).
    Returns (parsed, ok)."""
    stripped = THINK_BLOCK_RE.sub("", raw)
    if stripped != raw:
        THINK_STRIP_COUNT["count"] += 1
    parsed, ok = parse_model_text(stripped)
    if ok:
        PARSE_STAGE["last"] = "vendored"
        return parsed, True
    fences = re.findall(LAST_FENCE_PATTERN, stripped, re.DOTALL)
    if fences:
        try:
            parsed = json.loads(fences[-1].strip())
            LAST_FENCE_COUNT["count"] += 1
            PARSE_STAGE["last"] = "last-fence"
            return parsed, True
        except json.JSONDecodeError:
            pass
    parsed, ok = parse_fallback(stripped)
    if ok:
        PARSE_FALLBACK_COUNT["count"] += 1
        PARSE_STAGE["last"] = "fallback-n1"
        return parsed, True
    PARSE_STAGE["last"] = "failed"
    return None, False


def validate_amended(items, parsed):
    """N5 validation: typography-normalize BOTH sides, then delegate to the
    vendored validator (the single decider — count equality + membership,
    unchanged semantics). Letter-level substitutions still fail."""
    if not isinstance(parsed, dict):
        return False
    results = parsed.get("results")
    if not isinstance(results, list):
        return False
    norm_items = [normalize_typography(i) for i in items]
    norm_results = [
        {**r, "rubric_item": normalize_typography(r.get("rubric_item", ""))}
        if isinstance(r, dict) else r
        for r in results
    ]
    return validate_batch_result(norm_items, {"results": norm_results})


def query_batch(items, task, blocked, paper, judge, max_retries):
    """Vendored query_rubric_batch semantics (retries, validation). Returns
    (results_list | None, usage_metadata)."""
    rubric_input = {"task": task, "rubric_items": items, "blocked": blocked}
    rubric_json = json.dumps(rubric_input, ensure_ascii=False, indent=2)
    for attempt in range(max_retries):
        try:
            prompt = PROMPT_TEMPLATE.format(paper=paper, rubric=rubric_json)
            raw = judge.call(prompt)
            if not raw:
                print(f"[warn] batch attempt {attempt+1}/{max_retries} returned empty text")
                continue
            parsed, ok = parse_amended(raw)
            if not ok:
                print(f"[warn] batch attempt {attempt+1}/{max_retries} JSON parse failed")
                continue
            if not validate_amended(items, parsed):
                raw_v = validate_batch_result(items, parsed)
                print(f"[warn] batch attempt {attempt+1}/{max_retries} amended validation "
                      f"failed: rubric_item mismatch or wrong count "
                      f"(raw vendored verdict: {raw_v})")
                continue
            return parsed["results"], {
                "promptTokenCount": 0, "candidatesTokenCount": 0,
                "totalTokenCount": 0, "thoughtsTokenCount": 0,
            }
        except Exception as e:  # noqa: BLE001 - vendored retry semantics
            print(f"[warn] batch attempt {attempt+1}/{max_retries} request error: {e}")
            continue
    return None, {}


def score_report(idx, report_path, content, judge, chunk_size, max_retries):
    """Vendored process_one_with_chunking semantics. Returns
    (result_dict | {"error": ...}, prompt_token_max)."""
    if not isinstance(content, dict):
        return {"error": "invalid rubric_content"}, 0
    task = content.get("task", "")
    rubric = content.get("rubric", {})
    blocked = content.get("blocked", {})

    all_items = []
    dimension_map = {}
    for dim in ["info_recall", "analysis", "presentation"]:
        items = rubric.get(dim, [])
        if isinstance(items, list):
            for item in items:
                all_items.append(item)
                dimension_map[item] = dim
    if not all_items:
        return {"error": "no rubric items"}, 0

    text_content = read_report_text(report_path)
    if text_content and len(text_content) > MAX_PAPER_CHARS:
        print(f"[info] idx={idx} text too long ({len(text_content)}), "
              f"truncating to {MAX_PAPER_CHARS}")
        text_content = text_content[:MAX_PAPER_CHARS]

    if chunk_size <= 0 or chunk_size >= len(all_items):
        batches = [all_items]
    else:
        batches = [all_items[i:i + chunk_size] for i in range(0, len(all_items), chunk_size)]
    print(f"[info] idx={idx} has {len(all_items)} rubric items, "
          f"{len(batches)} batches (chunk_size={chunk_size})")

    all_results = []
    prompt_token_max = 0
    for batch_idx, batch_items in enumerate(batches):
        print(f"[info] idx={idx} batch {batch_idx+1}/{len(batches)}")
        results, usage = query_batch(batch_items, task, blocked, text_content,
                                     judge, max_retries)
        if results is None:
            return {"error": f"batch {batch_idx+1} failed after {max_retries} retries"}, prompt_token_max
        all_results.extend(results)
        pt = usage.get("promptTokenCount", 0)
        if pt > prompt_token_max:
            prompt_token_max = pt

    scores_by_dimension = {"info_recall": {}, "analysis": {}, "presentation": {}}
    for result in all_results:
        item = result.get("rubric_item", "")
        dim = dimension_map.get(item)
        if dim:
            scores_by_dimension[dim][item] = {
                "score": result.get("score", 0),
                "reason": result.get("reason", ""),
                "evidence": result.get("evidence", ""),
            }
    return {
        "task": task,
        "scores": scores_by_dimension,
        "usage_summary": {"input_tokens": 0, "output_tokens": 0,
                          "thoughts_tokens": 0, "total_tokens": 0},
        "usage_metadata_per_batch": [],
    }, prompt_token_max


# --------------------------------------------------------------------------
# Persistence (per-rubric rows land as the official result.jsonl lines)
# --------------------------------------------------------------------------
def result_path(results_dir: Path, model: str, idx: int) -> Path:
    return results_dir / f"{model}-idx-{idx}.jsonl"


def load_scored(results_dir: Path, model: str, idx: int):
    p = result_path(results_dir, model, idx)
    if not p.exists():
        return None
    lines = [json.loads(l) for l in open(p, encoding="utf-8")]
    for line in lines:
        if line.get("model") == model and int(line.get("idx")) == idx and "error" not in line.get("result", {}):
            return line["result"]
    return None


def persist_result(results_dir: Path, model: str, idx: int, result_dict: dict):
    p = result_path(results_dir, model, idx)
    p.parent.mkdir(parents=True, exist_ok=True)
    with open(p, "a", encoding="utf-8") as f:
        f.write(json.dumps({"model": model, "idx": idx, "result": result_dict}, ensure_ascii=False) + "\n")


# --------------------------------------------------------------------------
# Aggregation + bootstrap (official aggregation; our bootstrap/verdicts)
# --------------------------------------------------------------------------
def per_task_dims(result_dict):
    """Official compute_dimension_averages on one report's result."""
    return compute_dimension_averages(result_dict)


def model_scores(scored):
    """scored: {idx: result_dict}. Returns per-idx dims and the model mean
    (official aggregate_scores.py semantics: per-dim pass rate per task,
    model score = mean over tasks, x100)."""
    per_idx = {}
    for idx, rd in scored.items():
        dims = per_task_dims(rd)
        per_idx[idx] = dims
    def mean_of(key):
        vals = [d.get(key) for d in per_idx.values() if d.get(key) is not None]
        if not vals:
            return None
        return sum(vals) / len(vals)
    return {
        "per_idx": per_idx,
        "total": mean_of("total"),
        "inforecall": mean_of("inforecall"),
        "analysis": mean_of("analysis"),
        "presentation": mean_of("presentation"),
        "blocked_rate": mean_of("blocked_rate"),
    }


def cluster_bootstrap(per_idx_totals, n=BOOTSTRAP_N, seed=BOOTSTRAP_SEED):
    """Cluster bootstrap over tasks: resample tasks with replacement,
    pooled ones/items per resample. Returns sorted rates."""
    rng = random.Random(seed)
    entries = [(t[0], t[1]) for t in per_idx_totals.values() if t[1] is not None]
    rates = []
    for _ in range(n):
        ones = items = 0
        for _t in range(len(entries)):
            o, i = entries[rng.randrange(len(entries))]
            ones += o
            items += i
        rates.append(ones / items if items else 0.0)
    rates.sort()
    return rates


def per_task_ones_items(result_dict):
    """(ones, items) per task from the official aggregation shape."""
    dims = per_task_dims(result_dict)
    if dims.get("total") is None:
        return None
    total_items = 0
    ones = 0
    for dim in ["inforecall", "analysis", "presentation"]:
        for item_val in result_dict.get("scores", {}).get(dim, {}).values():
            if isinstance(item_val, dict) and isinstance(item_val.get("score"), (int, float)):
                total_items += 1
                if item_val["score"] == 1:
                    ones += 1
    return (ones, total_items)


def ci95(rates):
    if not rates:
        return None, None
    lo = rates[int(0.025 * len(rates))]
    hi = rates[int(0.975 * len(rates)) - 1]
    return lo, hi


def verdict_on_delta(lo, hi):
    """Leg A: the paired-cluster-bootstrap CI on the per-task delta
    (ours - perplexity, same judge, same tasks). met if CI_lo > 0;
    failed if CI_hi <= 0; could-not-judge otherwise."""
    if lo is None or hi is None:
        return "could-not-judge"
    if lo > 0:
        return "met"
    if hi <= 0:
        return "failed"
    return "could-not-judge"


def paired_delta_ci(ours_ones_items, ref_ones_items, n=BOOTSTRAP_N, seed=BOOTSTRAP_SEED):
    """Paired cluster bootstrap on the per-task TotalScore delta
    (ours - ref). Tasks are resampled jointly; each resample recomputes
    both pooled rates over the same task draw. Returns (sorted_deltas)."""
    idxs = sorted(set(ours_ones_items) & set(ref_ones_items))
    rng = random.Random(seed)
    deltas = []
    for _ in range(n):
        o = i_o = r = i_r = 0
        for _t in range(len(idxs)):
            j = idxs[rng.randrange(len(idxs))]
            o1, n1 = ours_ones_items[j]
            r1, n2 = ref_ones_items[j]
            o += o1; i_o += n1
            r += r1; i_r += n2
        deltas.append((o / i_o if i_o else 0.0) - (r / i_r if i_r else 0.0))
    deltas.sort()
    return deltas


# --------------------------------------------------------------------------
# Calibration channels (M1/M2/M3 — pre-registered §5)
# --------------------------------------------------------------------------
def m1_read(perp_scores, qwen_scores):
    """M1: same-judge, same-task, cross-model gap + Presentation ordering.
    Returns (measured_gap, ordering_holds, within_tolerance)."""
    if perp_scores["total"] is None or qwen_scores["total"] is None:
        return None, None, False
    gap = (qwen_scores["total"] - perp_scores["total"]) * 100.0
    ordering_holds = (perp_scores["presentation"] or 0.0) > (qwen_scores["presentation"] or 0.0)
    within = abs(gap - OFFICIAL_GAP) <= M1_GAP_TOLERANCE
    return gap, ordering_holds, within


def m2_read(scores):
    """M2: scale band check — Presentation rates must land in [60, 100]."""
    vals = [d.get("presentation") for d in scores["per_idx"].values()]
    vals = [v for v in vals if v is not None]
    if not vals:
        return None, False
    lo, hi = min(vals) * 100.0, max(vals) * 100.0
    return (lo, hi), (lo >= M2_BAND[0] and hi <= M2_BAND[1])


def m3_blocked_channel(scored, tasks):
    """M3(a): every -1 judgment's evidence must name the blocked title or
    one of its urls. Returns (errors, total_minus_ones, err_rows)."""
    errors, total = [], 0
    for idx, rd in scored.items():
        content = tasks.get(idx, {})
        blocked = content.get("blocked", {}) or {}
        keys = [s for s in [blocked.get("title", ""), *blocked.get("urls", [])] if s]
        for dim in ["info_recall", "analysis", "presentation"]:
            for item_val in rd.get("scores", {}).get(dim, {}).values():
                if not isinstance(item_val, dict):
                    continue
                if item_val.get("score") == -1:
                    total += 1
                    ev = item_val.get("evidence", "")
                    if not any(k and k in ev for k in keys):
                        errors.append({"idx": idx, "rubric_item": None,
                                       "evidence": ev[:200]})
    return errors, total


def m3_evidence_fidelity(scored):
    """M3(b): each non-empty evidence string must appear (whitespace-
    normalized) in the judged report. The scorer does not retain the
    report text here; the caller passes report_text. Returns
    (errors, checked)."""
    # implemented in score_all via report_text map — see m3_evidence_fidelity_text
    return [], 0


def m3_evidence_fidelity_text(scored, report_text):
    """M3(b) with the judged report text at hand."""
    errors, checked = [], 0
    norm = re.compile(r"\s+")
    text_n = norm.sub(" ", report_text)
    for idx, rd in scored.items():
        for dim in ["info_recall", "analysis", "presentation"]:
            for item_val in rd.get("scores", {}).get(dim, {}).values():
                if not isinstance(item_val, dict):
                    continue
                ev = item_val.get("evidence", "")
                if ev:
                    checked += 1
                    if norm.sub(" ", ev) not in text_n:
                        errors.append({"idx": idx, "evidence": ev[:200]})
    return errors, checked


def m3_repeat_self_consistency(judge, items, task, blocked, paper, max_retries):
    """M3(c): re-judge REPEAT_SELF_CONSISTENCY_N rubric items twice;
    score agreement rate. Deterministic item pick: caller passes the
    items; the two passes use identical prompts."""
    if len(items) < 2:
        return None, None
    r1, _ = query_batch(items, task, blocked, paper, judge, max_retries)
    r2, _ = query_batch(items, task, blocked, paper, judge, max_retries)
    if r1 is None or r2 is None:
        return None, None
    s1 = {r.get("rubric_item"): r.get("score") for r in r1}
    s2 = {r.get("rubric_item"): r.get("score") for r in r2}
    agree = sum(1 for k in s1 if k in s2 and s1[k] == s2[k])
    return agree, len(s1)


# --------------------------------------------------------------------------
# Seat-PAUSE protocol (t7c restart choreography, 2026-08-19)
# --------------------------------------------------------------------------
def _paused(results_dir) -> bool:
    """True when results_dir/PAUSE exists — the seat's PAUSE signal.

    The scorer checks this only BETWEEN reports (persistence granularity:
    a report's results are written at its end, never mid-report), so a
    pause lands cleanly: the in-flight report finishes, its results are
    persisted, and the process exits 0. Nothing is lost and nothing is
    re-scored; a resumed run loads completed reports via load_scored.
    Stop latency bound: one report (~13-45 min worst case).
    """
    return (Path(results_dir) / "PAUSE").exists()


# --------------------------------------------------------------------------
# Score a whole report set
# --------------------------------------------------------------------------
def score_set(reports_dir, tasks, judge, results_dir, idxs, model_name,
              chunk_size=CHUNK_SIZE, max_retries=MAX_RETRIES, force=False):
    """Score every report under reports_dir/<model>/idx-N.md for N in idxs.
    Persists official-shape result lines. Returns {idx: result_dict}."""
    scored = {}
    for idx in sorted(idxs):
        if _paused(results_dir):
            print("[pause] PAUSE marker present — clean stop at report "
                  "boundary (completed reports are persisted)")
            break
        rp = reports_dir / model_name / f"idx-{idx}.md"
        if not rp.exists():
            print(f"[skip] {rp} missing")
            continue
        existing = None if force else load_scored(results_dir, model_name, idx)
        if existing is not None:
            print(f"[resume] {model_name} idx-{idx} already scored")
            scored[idx] = existing
            continue
        rd, ptx = score_report(idx, rp, tasks.get(idx, {}), judge,
                               chunk_size, max_retries)
        if "error" in rd:
            print(f"[err] {model_name} idx-{idx}: {rd['error']}")
            continue
        persist_result(results_dir, model_name, idx, rd)
        scored[idx] = rd
        if ptx > PROMPT_TOKEN_BUDGET:
            print(f"[warn] idx={idx} prompt reached {ptx} tokens — lower "
                  f"DRB2_CHUNK_SIZE (amendment N4 default 4) and rerun")
    return scored


# --------------------------------------------------------------------------
# Report
# --------------------------------------------------------------------------
def render_verdict_table(perp_scored, qwen_scored, ours_scored, tasks, judge,
                         results_dir, reports_dir, idxs):
    lines = []
    def s100(x):
        return None if x is None else round(x * 100.0, 2)
    rows = []
    for idx in sorted(idxs):
        def dims_of(scored):
            if idx not in scored:
                return {}
            return per_task_dims(scored[idx])
        p, q, o = dims_of(perp_scored), dims_of(qwen_scored), dims_of(ours_scored)
        rows.append((idx, p, q, o))
    lines.append("| idx | Perp (T/IR/A/P) | Qwen (T/IR/A/P) | ours (T/IR/A/P) |")
    lines.append("|---|---|---|---|")
    for idx, p, q, o in rows:
        def fmt(d):
            if not d:
                return "-"
            return " ".join(f"{s100(d.get(k))}" for k in
                            ["total", "inforecall", "analysis", "presentation"])
        lines.append(f"| {idx} | {fmt(p)} | {fmt(q)} | {fmt(o)} |")

    def model_line(name, scored):
        ms = model_scores(scored)
        return (f"{name}: Total {s100(ms['total'])} "
                f"(IR {s100(ms['inforecall'])} / A {s100(ms['analysis'])} / "
                f"P {s100(ms['presentation'])}) blocked_rate "
                f"{s100(ms['blocked_rate'])}")
    lines.append("")
    lines.append(model_line("Perplexity-Research", perp_scored))
    lines.append(model_line("Qwen-3-Max-DeepResearch", qwen_scored))
    lines.append(model_line("ours", ours_scored))
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description="DRB-II scorer (t7a)")
    ap.add_argument("--reports-dir", default=str(Path(__file__).parent / "reports"))
    ap.add_argument("--tasks", default="/home/alexbryan/dev/DeepResearch-Bench-II/tasks_and_rubrics.jsonl")
    ap.add_argument("--results-dir", default=str(Path(__file__).parent / "results"))
    ap.add_argument("--selection", default=str(Path(__file__).parent / "selection.json"),
                    help="selection.json (drawn idx list; scorer restricts to it)")
    ap.add_argument("--chunk-size", type=int, default=CHUNK_SIZE)
    ap.add_argument("--max-retries", type=int, default=MAX_RETRIES)
    ap.add_argument("--models", nargs="*", default=MODELS)
    ap.add_argument("--force", action="store_true", help="re-score already-scored reports")
    ap.add_argument("--calibrate", action="store_true",
                    help="run M1/M2/M3 calibration after scoring (pre-registered §5)")
    ap.add_argument("--repeat-items", type=int, default=REPEAT_SELF_CONSISTENCY_N,
                    help="M3(c) rubric items re-judged twice")
    ap.add_argument("--selftest", action="store_true", help="run the selftest (mock judge)")
    args = ap.parse_args()

    if args.selftest:
        sys.exit(selftest())

    tasks = load_tasks(args.tasks)
    selection = json.load(open(args.selection, encoding="utf-8"))
    idxs = [int(d["idx"]) for d in selection["draws"]]
    judge = Judge()

    reports_dir = Path(args.reports_dir)
    results_dir = Path(args.results_dir)
    results_dir.mkdir(parents=True, exist_ok=True)

    scored = {}
    for model in args.models:
        scored[model] = score_set(reports_dir, tasks, judge, results_dir,
                                  idxs, model, chunk_size=args.chunk_size,
                                  max_retries=args.max_retries, force=args.force)

    if _paused(results_dir):
        print("[pause] clean stop after scoring — resumed runs load completed "
              "reports via load_scored")
        sys.exit(0)

    perp_scored, qwen_scored, ours_scored = (
        scored.get("Perplexity-Research", {}),
        scored.get("Qwen-3-Max-DeepResearch", {}),
        scored.get("ours", {}),
    )

    # --- Leg A: ours vs Perplexity, same judge, same tasks ----------------
    ours_oi = {i: per_task_ones_items(rd) for i, rd in ours_scored.items()}
    perp_oi = {i: per_task_ones_items(rd) for i, rd in perp_scored.items()}
    common = sorted(set(ours_oi) & set(perp_oi))
    common = [i for i in common if ours_oi[i] and perp_oi[i]]
    leg_a = {"state": "never-ran", "verdict": "never-ran", "n_tasks": len(common)}
    if common:
        deltas = paired_delta_ci({i: ours_oi[i] for i in common},
                                 {i: perp_oi[i] for i in common})
        lo, hi = ci95(deltas)
        leg_a = {
            "state": "scored",
            "verdict": verdict_on_delta(lo, hi),
            "delta_ci": {"lo": round(lo * 100.0, 2), "hi": round(hi * 100.0, 2)},
            "n_tasks": len(common),
            "seed_string": BOOTSTRAP_SEED_STRING,
            "n_resamples": BOOTSTRAP_N,
        }

    # --- Leg B / C: descriptive numbers with caveats ----------------------
    ours_ms = model_scores(ours_scored)
    perp_ms = model_scores(perp_scored)
    qwen_ms = model_scores(qwen_scored)

    report = {
        "instrument": {
            "judge_model": JUDGE_MODEL,
            "daemon_url": DAEMON_URL,
            "max_paper_chars": MAX_PAPER_CHARS,
            "chunk_size": args.chunk_size,
            "max_retries": args.max_retries,
            "output_token_budget": OUTPUT_TOKEN_BUDGET,
            "reasoning_effort": judge.reasoning_effort if not judge.mock else "mock",
            "reasoning_effort_strip_events": judge.strip_events,
            "parse_fallback_count_amendment_n1": PARSE_FALLBACK_COUNT["count"],
            "think_strip_count_amendment_n5": THINK_STRIP_COUNT["count"],
            "last_fence_count_amendment_n5": LAST_FENCE_COUNT["count"],
            "amendments": [
                "N1 parse fallback (2026-08-19)",
                "N5 typography-normalized validation + parse robustness (2026-08-20)",
            ],
            "vendored": "vendor/ (byte-exact, SHA256SUMS; N5 normalizes inputs "
                        "to the vendored validator, which remains the sole decider)",
        },
        "sample": {"n": len(idxs), "idxs": idxs,
                   "seed_string": selection.get("seed_string"),
                   "selection_file": str(Path(args.selection))},
        "leg_a": leg_a,
        "leg_b": {
            "ours": ours_ms,
            "reference_perplexity_official": OFFICIAL_PERPLEXITY,
            "reference_perplexity_en_official": OFFICIAL_PERPLEXITY_EN,
            "reference_nvidia_aiq_official": {"total": 54.50, "ir": 49.23,
                                               "analysis": 61.55, "presentation": 93.15},
            "caveat": ("cross-judge (our 27B vs official GPT-5.5) AND "
                       "cross-task-set (8 sampled en tasks vs official 132); "
                       "descriptive only, never a gate"),
        },
        "leg_c": {
            "ours_blocked_rate": ours_ms["blocked_rate"],
            "perplexity_blocked_rate": perp_ms["blocked_rate"],
            "qwen_blocked_rate": qwen_ms["blocked_rate"],
            "note": ("the -1 channel measures reliance on the blocked expert "
                     "articles; our loop cannot cite them, so near-zero is "
                     "expected on all three sets — measured, never assumed"),
        },
        "models_scored": {m: len(scored[m]) for m in args.models},
    }

    # --- Calibration (pre-registered §5) ----------------------------------
    if args.calibrate and not judge.mock:
        cal = {"m1": {}, "m2": {}, "m3": {}}
        gap, ordering, within = m1_read(perp_ms, qwen_ms)
        cal["m1"] = {
            "measured_gap_qwen_minus_perplexity": gap,
            "official_gap": OFFICIAL_GAP,
            "presentation_ordering_holds": ordering,
            "within_tolerance_plus_minus_5": within,
            "acceptance": ("HOLD" if (within and ordering) else "OFF-SCALE"),
            "official_lines": {"perplexity": OFFICIAL_PERPLEXITY,
                               "qwen": OFFICIAL_QWEN},
        }
        band, in_band = m2_read(qwen_ms)
        perp_band, perp_in = m2_read(perp_ms)
        cal["m2"] = {
            "band": M2_BAND,
            "qwen_presentation_lo_hi": band,
            "qwen_in_band": in_band,
            "perplexity_presentation_lo_hi": perp_band,
            "perplexity_in_band": perp_in,
            "acceptance": ("PASS" if (in_band and perp_in) else "SCALE-DRIFT"),
        }
        errs, total = m3_blocked_channel(perp_scored, tasks)
        cal["m3"]["blocked_channel"] = {
            "errors": len(errs), "total_minus_ones": total,
            "allowed": M3_BLOCKED_ERRORS_ALLOWED,
            "pass": len(errs) <= M3_BLOCKED_ERRORS_ALLOWED,
            "error_rows": errs[:5],
        }
        # evidence fidelity over the two fixture sets
        ev_errs, ev_checked = 0, 0
        for model in ["Perplexity-Research", "Qwen-3-Max-DeepResearch"]:
            for idx in idxs:
                rp = reports_dir / model / f"idx-{idx}.md"
                if rp.exists():
                    e, c = m3_evidence_fidelity_text(
                        {idx: scored[model].get(idx)}, read_report_text(rp))
                    ev_errs += len(e); ev_checked += c
        cal["m3"]["evidence_fidelity"] = {
            "errors": ev_errs, "checked": ev_checked,
            "rate": (1 - ev_errs / ev_checked) if ev_checked else None,
            "floor": M3_EVIDENCE_FIDELITY_FLOOR,
            "pass": (ev_checked > 0 and (1 - ev_errs / ev_checked) >= M3_EVIDENCE_FIDELITY_FLOOR),
        }
        # repeat self-consistency: first task's first N items
        rep_model = "Perplexity-Research"
        if rep_model in scored and scored[rep_model]:
            rep_idx = min(scored[rep_model])
            content = tasks.get(rep_idx, {})
            all_items = []
            for dim in ["info_recall", "analysis", "presentation"]:
                all_items += content.get("rubric", {}).get(dim, [])
            pick = all_items[:args.repeat_items]
            rp = reports_dir / rep_model / f"idx-{rep_idx}.md"
            paper = read_report_text(rp)
            agree, n = m3_repeat_self_consistency(
                judge, pick, content.get("task", ""), content.get("blocked", {}),
                paper, args.max_retries)
            cal["m3"]["repeat_self_consistency"] = {
                "agree": agree, "n": n,
                "rate": (agree / n) if n else None,
                "floor": M3_REPEAT_AGREEMENT_FLOOR,
                "pass": (n and agree / n >= M3_REPEAT_AGREEMENT_FLOOR),
            }
        report["calibration"] = cal

    if _paused(results_dir):
        print("[pause] clean stop before report write — M3(c) is not persisted; "
              "a resumed run re-runs it (~20 min)")
        sys.exit(0)

    out = Path(args.results_dir) / "drb2-report.json"
    out.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print("=" * 78)
    print(render_verdict_table(perp_scored, qwen_scored, ours_scored, tasks,
                               judge, results_dir, reports_dir, idxs))
    print("=" * 78)
    print(json.dumps(report, indent=2, ensure_ascii=False))
    print(f"[done] report written to {out}")


# --------------------------------------------------------------------------
# Selftest (mock judge; no daemon, no network)
# --------------------------------------------------------------------------
def selftest():
    os.environ["DRB2_JUDGE"] = "mock"
    judge = Judge()
    assert judge.mock

    # two fake tasks, three dims each
    tasks = {
        1: {"task": "task one",
            "rubric": {"info_recall": ["known-true fact A", "known-absent fact B"],
                       "analysis": ["known-true analysis C"],
                       "presentation": ["known-absent section D"]},
            "blocked": {"title": "Blocked Expert Article", "authors": ["X"], "urls": ["http://x"]}},
        2: {"task": "task two",
            "rubric": {"info_recall": ["known-true fact E", "known-blocked fact F"],
                       "analysis": ["known-true analysis G", "known-absent H"],
                       "presentation": ["known-true section I"]},
            "blocked": {"title": "Second Blocked Article", "authors": ["Y"], "urls": ["http://y"]}},
    }
    tmp = Path("/tmp/drb2-selftest")
    import shutil
    shutil.rmtree(tmp, ignore_errors=True)
    rep = tmp / "reports"
    for model in MODELS:
        (rep / model).mkdir(parents=True)
    for idx, t in tasks.items():
        for model in MODELS:
            (rep / model / f"idx-{idx}.md").write_text(
                f"Report {idx}\n\nevidence-true\n"
                f"citing \"{t['blocked']['title']}\"\n",
                encoding="utf-8")

    scored = {}
    for model in MODELS:
        rd1, _ = score_report(1, rep / model / "idx-1.md", tasks[1], judge,
                              CHUNK_SIZE, MAX_RETRIES)
        rd2, _ = score_report(2, rep / model / "idx-2.md", tasks[2], judge,
                              CHUNK_SIZE, MAX_RETRIES)
        assert "error" not in rd1 and "error" not in rd2
        scored[model] = {1: rd1, 2: rd2}

    # hand-computed expectations (mock judge semantics)
    # task 1: IR 1/2, A 1/1, P 0/1 -> total 2/4
    d1 = per_task_dims(scored["ours"][1])
    assert abs(d1["inforecall"] - 0.5) < 1e-9, d1
    assert abs(d1["analysis"] - 1.0) < 1e-9
    assert abs(d1["presentation"] - 0.0) < 1e-9
    assert abs(d1["total"] - 0.5) < 1e-9
    assert abs(d1["blocked_rate"] - 0.0) < 1e-9
    # task 2: IR 1/2 (one blocked), A 1/2, P 1/1 -> total 3/5, blocked 1/5
    d2 = per_task_dims(scored["ours"][2])
    assert abs(d2["inforecall"] - 0.5) < 1e-9
    assert abs(d2["analysis"] - 0.5) < 1e-9
    assert abs(d2["presentation"] - 1.0) < 1e-9
    assert abs(d2["total"] - 0.6) < 1e-9, d2
    assert abs(d2["blocked_rate"] - 0.2) < 1e-9

    ms = model_scores(scored["ours"])
    assert abs(ms["total"] - (0.5 + 0.6) / 2) < 1e-9
    assert abs(ms["inforecall"] - 0.5) < 1e-9
    assert abs(ms["analysis"] - 0.75) < 1e-9
    assert abs(ms["presentation"] - 0.5) < 1e-9

    # Leg A verdict logic on a synthetic delta
    assert verdict_on_delta(0.01, 0.9) == "met"
    assert verdict_on_delta(-0.9, -0.01) == "failed"
    assert verdict_on_delta(-0.3, 0.3) == "could-not-judge"
    assert verdict_on_delta(None, None) == "could-not-judge"

    # bootstrap determinism
    oi = {1: (2, 4), 2: (3, 5)}
    pi = {1: (1, 4), 2: (2, 5)}
    a = paired_delta_ci(oi, pi)
    b = paired_delta_ci(oi, pi)
    assert a == b, "paired delta bootstrap must be seed-deterministic"
    lo, hi = ci95(a)
    assert lo is not None and hi is not None and lo <= hi

    # M3(a): blocked channel — task 2 has a -1 with evidence naming the
    # blocked title (clean); a fabricated -1 must be caught
    errs, total = m3_blocked_channel(scored["ours"], tasks)
    assert total == 1 and errs == [], (total, errs)
    bad = {2: {"scores": {"info_recall": {"known-blocked fact F": {"score": -1, "evidence": "no blocked name"}}}}}
    errs2, _ = m3_blocked_channel(bad, tasks)
    assert len(errs2) == 1

    # M3(b): evidence fidelity — mock evidence cites the report's content
    full_text = (rep / "ours" / "idx-1.md").read_text(encoding="utf-8") + \
                (rep / "ours" / "idx-2.md").read_text(encoding="utf-8")
    e, c = m3_evidence_fidelity_text(scored["ours"], full_text)
    assert c > 0 and e == [], (e, c)

    # vendored validation: wrong count must fail
    from parse_validate import validate_batch_result
    assert not validate_batch_result(["a", "b"], {"results": [{"rubric_item": "a"}]})
    assert not validate_batch_result(["a"], {"results": [{"rubric_item": "b"}]})
    assert validate_batch_result(["a"], {"results": [{"rubric_item": "a"}]})

    # amendment N1: compact single-line JSON fails the vendored parser
    # (official _try_clean_and_load corrupts '", "key":' adjacencies —
    # measured) and must parse via the counted fallback
    compact = json.dumps({"results": [{"rubric_item": "a", "score": 1,
                                       "reason": "m", "evidence": "e"}]})
    p1, ok1 = parse_model_text(compact)
    assert not ok1, "vendored parser must fail on compact JSON (measured property)"
    p2, ok2 = parse_fallback(compact)
    assert ok2 and p2["results"][0]["rubric_item"] == "a"
    before = PARSE_FALLBACK_COUNT["count"]
    qb, _ = query_batch(["a"], "t", {}, "paper", judge, MAX_RETRIES)
    assert qb is not None and PARSE_FALLBACK_COUNT["count"] == before, (
        "pretty mock output must NOT hit the fallback")

    # retry path: 'known-parse-error' first call is non-JSON, second is valid
    # (fresh judge so the odd/even call parity is deterministic)
    judge2 = Judge()
    rd_retry, _ = score_report(1, rep / "ours" / "idx-1.md",
                               {**tasks[1], "task": "known-parse-error " + tasks[1]["task"]},
                               judge2, CHUNK_SIZE, MAX_RETRIES)
    assert "error" not in rd_retry
    assert judge2.mocked_calls >= 2, judge2.mocked_calls

    # seat-PAUSE protocol: a PAUSE marker stops score_set before any report
    # (checkpointed between reports; nothing is judged or persisted after it)
    tmp_pause = Path("/tmp/drb2-selftest-pause")
    shutil.rmtree(tmp_pause, ignore_errors=True)
    res_pause = tmp_pause / "results"
    res_pause.mkdir(parents=True)
    (res_pause / "PAUSE").write_text("")
    judge3 = Judge()
    paused_scored = score_set(rep, tasks, judge3, res_pause, [1, 2], "ours",
                              chunk_size=CHUNK_SIZE, max_retries=MAX_RETRIES)
    assert paused_scored == {}, f"PAUSE marker must stop before any report: {paused_scored}"
    assert judge3.mocked_calls == 0, "no judge calls may fire after PAUSE"
    (res_pause / "PAUSE").unlink()

    # amendment N5: typography-normalized validation — the same claim in any
    # typography (case, whitespace, quote style, markdown decorations) must
    # pass; a letter-level substitution must still fail
    orig = ("Explicitly state that Indonesia's 'Taspen - JP' is a mandatory "
            "Pay-As-You-Go Defined Benefit (PAYG DB) scheme for public sector "
            "employees.")
    drifted = ("Explicitly state thatIndonesia's \"Taspen - JP\"isa mandatory "
               "**Pay-As-You-Go** Defined Benefit(PAYG **DB**) scheme for "
               "public  sector employees.")
    assert normalize_typography(orig) == normalize_typography(drifted), \
        "N5 typography equivalence"
    items4 = [orig, "x", "y", "z"]
    payload4 = {"results": [
        {"rubric_item": drifted, "score": 1, "reason": "r", "evidence": "e"},
        {"rubric_item": "x", "score": 0, "reason": "r", "evidence": ""},
        {"rubric_item": "y", "score": 0, "reason": "r", "evidence": ""},
        {"rubric_item": "z", "score": 0, "reason": "r", "evidence": ""},
    ]}
    assert validate_amended(items4, payload4), "N5 drifted typography must pass"
    assert not validate_batch_result(items4, payload4), \
        "vendored validator must still fail on the drift (unchanged decider)"
    sub_payload = {"results": [
        {**payload4["results"][0],
         "rubric_item": payload4["results"][0]["rubric_item"].replace("Taspen", "Tashen")},
    ] + payload4["results"][1:]}
    assert not validate_amended(items4, sub_payload), \
        "N5: letter-level substitution must still fail"
    assert not validate_amended(["a", "b"], {"results": [{"rubric_item": "a"}]}), \
        "N5: count mismatch must still fail"

    # N5 parse: <think> prefix with fences inside the think block; the valid
    # payload sits after </think> (the 122b-2r failure shape)
    think_raw = ("<think>Let me consider.\n```json\n{\"results\": [{\"rubric_item\": "
                 "\"inner\"}]}\n```\nI need to check the facts.\n</think>\n\n"
                 + json.dumps({"results": [
                     {"rubric_item": "a", "score": 1, "reason": "r", "evidence": "e"},
                     {"rubric_item": "b", "score": 0, "reason": "r", "evidence": ""}]},
                     indent=1))
    p5, ok5 = parse_amended(think_raw)
    assert ok5 and len(p5["results"]) == 2 and p5["results"][0]["rubric_item"] == "a", \
        "N5 think-strip parse must recover the payload after </think>"
    # N5 parse: first fence garbage, last fence valid -> last-fence attempt
    two_fence = ("```json\nnot json at all\n```\n```json\n"
                 + json.dumps({"results": [{"rubric_item": "a"}]}) + "\n```")
    p6, ok6 = parse_amended(two_fence)
    assert ok6 and p6["results"][0]["rubric_item"] == "a", "N5 last-fence parse"

    print("selftest: ALL PASS")
    return 0


if __name__ == "__main__":
    main()
