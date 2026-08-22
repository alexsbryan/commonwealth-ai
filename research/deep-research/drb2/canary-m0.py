#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M0 canary (order deep-research-t7a, amendment N3).

One real rubric batch through the real judge path (drb2-score.Judge):
10 rubric items of idx-4 (Finance & Business) over the Perplexity
fixture report, truncated to 15K chars. Prints, line by line (flush):
  PROMPT_CHARS <n>
  CALL_SECS <n.n>            (client-observed wall time of the request)
  STRIP_EVENTS <json>        (reasoning_effort strip events, deviation #5)
  USAGE <json>               (daemon usage: prompt/completion tokens)
  VENDORED_PARSE <bool>
  RESULTS <n> VALID <bool> SCORES <counter json>
  FALLBACK_PARSE <bool>      (only if vendored parse failed)
  M0_CANARY_DONE

Run WITHOUT any outer kill wrapper: the client's own bound is
DRB2_CLIENT_TIMEOUT (default 2700s, amendment N3). One attempt; the
vendored retry loop belongs to the scorer, not this probe.
"""

import importlib.util
import json
import sys
import time
from pathlib import Path

spec = importlib.util.spec_from_file_location("drb2", "drb2-score.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

tasks = m.load_tasks("/home/alexbryan/dev/DeepResearch-Bench-II/tasks_and_rubrics.jsonl")
content = tasks[4]
items = []
for dim in ["info_recall", "analysis", "presentation"]:
    items += content["rubric"].get(dim, [])
batch = items[:10]
paper = m.read_report_text(Path(__file__).parent / "reports/Perplexity-Research/idx-4.md")
paper = paper[:15000]
rubric_json = json.dumps({"task": content["task"], "rubric_items": batch,
                          "blocked": content.get("blocked", {})},
                         ensure_ascii=False, indent=2)
prompt = m.PROMPT_TEMPLATE.format(paper=paper, rubric=rubric_json)
print(f"PROMPT_CHARS {len(prompt)}", flush=True)

judge = m.Judge()
assert not judge.mock
t0 = time.time()
raw = judge.call(prompt)
call_secs = time.time() - t0
print(f"CALL_SECS {call_secs:.1f}", flush=True)
print(f"STRIP_EVENTS {json.dumps(judge.strip_events)}", flush=True)
print(f"USAGE {json.dumps(judge.last_usage)}", flush=True)
usage = judge.last_usage or {}
comp = usage.get("completion_tokens") or 0
if comp and call_secs > 0:
    print(f"TOKPS {comp / call_secs:.2f} PER_ITEM_TOKENS {comp / len(batch):.1f} "
          f"TOTAL_OUTPUT_TOKENS {comp}", flush=True)
else:
    print("TOKPS unavailable", flush=True)

from parse_validate import parse_model_text, validate_batch_result
parsed, ok = parse_model_text(raw)
print(f"VENDORED_PARSE {ok}", flush=True)
if ok:
    res = parsed["results"]
    from collections import Counter
    print(f"RESULTS {len(res)} VALID {validate_batch_result(batch, parsed)} "
          f"SCORES {json.dumps(dict(Counter(r['score'] for r in res)))}", flush=True)
else:
    p2, ok2 = m.parse_fallback(raw)
    print(f"FALLBACK_PARSE {ok2}", flush=True)
    if ok2:
        print(f"RESULTS {len(p2['results'])} VALID "
              f"{validate_batch_result(batch, p2)}", flush=True)
print("M0_CANARY_DONE", flush=True)
