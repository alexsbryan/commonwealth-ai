#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M0b diagnostic canary (order deep-research-t7a): same batch as canary-m0,
but SAVES the raw judge output to a scratch file and prints format
diagnostics. Purpose: the m0 canary's raw output failed BOTH parse paths
(vendored + N1 fallback); we need the actual output shape to decide the
parse-path fix vs an order-level format finding. No outer kill wrapper;
client bound DRB2_CLIENT_TIMEOUT (default 2700s).
"""

import importlib.util
import json
import os
import re
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
_off = int(os.environ.get("DRB2_CANARY_OFFSET", "0"))
_n = int(os.environ.get("DRB2_CANARY_ITEMS", "10"))
batch = items[_off:_off + _n]
print(f"BATCH_ITEMS {len(batch)} BATCH_OFFSET {_off}", flush=True)
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

# save the raw output — the point of this canary
out = Path(__file__).parent / "results" / f"canary-m0b-raw-{_off}-{_n}.txt"
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(raw, encoding="utf-8")
print(f"RAW_SAVED {out} RAW_CHARS {len(raw)}", flush=True)

# format diagnostics
print(f"RAW_START {raw[:1500]!r}", flush=True)
print(f"RAW_END {raw[-800:]!r}", flush=True)
fences = re.findall(r"```[a-zA-Z]*", raw)
print(f"FENCES {fences}", flush=True)
print(f"COUNT_LBRACE {raw.count('{')} RBRACE {raw.count('}')} "
      f"LBRACKET {raw.count('[')} RBRACKET {raw.count(']')}", flush=True)
has_results = "results" in raw
print(f"HAS_RESULTS_KEY {has_results}", flush=True)
# try JSONDecoder.raw_decode at start and after leading prose
dec = json.JSONDecoder()
try:
    obj, end = dec.raw_decode(raw.lstrip())
    print(f"RAW_DECODE_START_OK end_at {end} of {len(raw)}", flush=True)
except json.JSONDecodeError as e:
    print(f"RAW_DECODE_START_FAIL {e}", flush=True)
    # is the failure early (corrupt) or late (truncated)? find the first bad spot
    print(f"DECODE_ERROR_POS {e.pos} first_bad_ctx {raw[max(0,e.pos-120):e.pos+120]!r}", flush=True)
# does it end cleanly?
print(f"ENDS_CLEAN {raw.rstrip().endswith('}') or raw.rstrip().endswith(']')}", flush=True)
print("M0B_CANARY_DONE", flush=True)
