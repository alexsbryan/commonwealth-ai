#!/usr/bin/env python3
"""t7a replay probe 1: one live-shaped batch through the scorer's exact client path.
Paper = reports/Perplexity-Research/idx-4.md truncated at MAX_PAPER_CHARS; items =
idx-4 info_recall 0-3; prompt built exactly as query_batch; judge.call() exactly as
the scorer; then vendored parse + validate. Saves raw + verdict to drb2/results/."""
import importlib.util, sys, json, time, pathlib, os

PAPER_MAX = int(os.environ.get("DRB2_PROBE_PAPER_MAX", "45000"))
TAG = os.environ.get("DRB2_PROBE_TAG") or time.strftime("%m%d-%H%M%S")

SPEC = importlib.util.spec_from_file_location(
    "drb2score", "/home/alexbryan/dev/commonwealth-ai/research/deep-research/drb2/drb2-score.py")
m = importlib.util.module_from_spec(SPEC); sys.modules["drb2score"] = m; SPEC.loader.exec_module(m)
spec2 = importlib.util.spec_from_file_location(
    "pv", "/home/alexbryan/dev/commonwealth-ai/research/deep-research/drb2/vendor/parse_validate.py")
pv = importlib.util.module_from_spec(spec2); sys.modules["pv"] = pv; spec2.loader.exec_module(pv)

results_dir = pathlib.Path("/home/alexbryan/dev/commonwealth-ai/research/deep-research/drb2/results")

rec = None
for line in open("/home/alexbryan/dev/DeepResearch-Bench-II/tasks_and_rubrics.jsonl"):
    r = json.loads(line)
    if r.get("idx") == 4:
        rec = r; break
content = rec["content"]
task, rubric, blocked = content["task"], content["rubric"], content["blocked"]
items = rubric["info_recall"][0:4]

report_path = "/home/alexbryan/dev/commonwealth-ai/research/deep-research/drb2/reports/Perplexity-Research/idx-4.md"
text_content = m.read_report_text(report_path)
truncated = len(text_content) > PAPER_MAX
if truncated:
    text_content = text_content[:PAPER_MAX]

rubric_input = {"task": task, "rubric_items": items, "blocked": blocked}
rubric_json = json.dumps(rubric_input, ensure_ascii=False, indent=2)
prompt = m.PROMPT_TEMPLATE.format(paper=text_content, rubric=rubric_json)

print(f"[probe] idx=4 items[0:4] paper_chars={len(text_content)} truncated={truncated} "
      f"rubric_chars={len(rubric_json)} prompt_chars={len(prompt)}", flush=True)

t0 = time.time()
try:
    raw = judge.call(prompt) if (judge := m.Judge()) else ""
except Exception as e:
    print(f"[probe] judge.call raised {type(e).__name__}: {e}", flush=True)
    raise SystemExit(2)
elapsed = time.time() - t0
print(f"[probe] judge.call returned after {elapsed:.1f}s raw_chars={len(raw) if raw else 0}", flush=True)
(results_dir / f"replay-probe-{TAG}-raw.txt").write_text(raw or "", encoding="utf-8")

if not raw:
    print("[probe] EMPTY raw", flush=True)
    raise SystemExit(3)

# Amendment N5 (pre-registered 2026-08-20): the amended parse/validate is
# THE GATE; the raw vendored outcomes print alongside for the record
# (glassbox) — never the gate.
parsed, ok = m.parse_amended(raw)
print(f"[probe] parse: amended ok={ok} stage={m.PARSE_STAGE['last']}", flush=True)
if ok:
    res = parsed.get("results", [])
    verdict = m.validate_amended(items, parsed)
    raw_verdict = pv.validate_batch_result(items, parsed)
    echoed = [r.get("rubric_item", "") for r in res]
    print(f"[probe] results={len(res)} amended_validate={verdict} "
          f"raw_vendored_validate={raw_verdict}", flush=True)
    for i, (exp, got) in enumerate(zip(items, echoed)):
        if exp != got:
            print(f"[probe]   MISMATCH {i}:\n    ORIG {exp[:120]!r}\n    ECHO {got[:120]!r}", flush=True)
    if len(res) != len(items):
        print(f"[probe]   COUNT {len(res)} != {len(items)}", flush=True)
    scores = [r.get("score") for r in res]
    print(f"[probe] scores={scores}", flush=True)
    raise SystemExit(0 if verdict else 4)
else:
    print(f"[probe] parse FAILED; raw head: {raw[:400]!r}", flush=True)
    raise SystemExit(5)
