#!/usr/bin/env python3
"""Single-task RACE scorer — the official recipe, one task at a time.

Uses the pinned clone's OWN modules (format_criteria_list, score_prompt_en,
calculate_weighted_scores, extract_json_from_markdown) so the arithmetic is
the benchmark's, not a re-implementation. The full-subset instrument stays
score_race.py; this exists so a single-task iteration costs ONE judge call
instead of ten.

  --replay FILE --task N   re-derive from a stored judge_output.jsonl row
                           (zero judge calls; the instrument-validation path)
  --article PATH --task N  one fresh judge call against the article
"""
import json, sys, os, argparse, time, urllib.request

BASE = 'http://127.0.0.1:9741/v1'

# The clone's driver imports `utils.api` transitively, and that module
# freezes LLM_BACKEND / base_url / api_key at IMPORT time. Setting these
# inside judge() was too late: the constants had already resolved to the
# vendored default (openrouter), and the first judge call went to an
# EXTERNAL provider carrying a real key. It 400'd on the local model id
# rather than silently scoring with a different judge — but a valid id
# would have bought a substituted instrument at real cost. Forced here,
# before any clone import, and guarded again at call time.
os.environ['LLM_BACKEND'] = 'openai'
os.environ['OPENAI_BASE_URL'] = BASE
os.environ.setdefault('OPENAI_API_KEY', 'local')
# The vendored client freezes its socket timeout at import (utils/api.py:82,
# `LLM_HTTP_TIMEOUT`, default 600s) and `judge()`'s own `timeout=` argument
# never reached it. Task 83 is the largest prompt in the subset — a ~40k-token
# reference plus our article — and at this host's prefill rate it needs ~8min
# before the first token. It timed out CLIENT-side at 600s and was recorded
# NEVER-RAN, which reads as a model refusal and is not one. A judge call is
# allowed to be slow; it is not allowed to be silently cut off.
os.environ.setdefault('LLM_HTTP_TIMEOUT', '3600')

CLONE = '/home/alexbryan/dev/deep_research_bench'
sys.path.insert(0, CLONE)
from deepresearch_bench_race import format_criteria_list          # noqa: E402
from prompt.score_prompt_en import generate_merged_score_prompt   # noqa: E402
from utils.score_calculator import calculate_weighted_scores      # noqa: E402
from utils.json_extractor import extract_json_from_markdown       # noqa: E402

DIMS = ["comprehensiveness", "insight", "instruction_following", "readability"]


# ── INSTRUMENT AMENDMENT 2026-08-23: the judge is made deterministic ────────
#
# The vendored client sends `model`, `messages`, `max_completion_tokens` and
# `reasoning_effort` — and NO sampling parameters (utils/api.py:167-172). The
# local daemon therefore applies its own default, and
# `InferenceConfig::default().temperature` is **0.7**
# (sovereign-contracts/src/types/mod.rs:107). Every RACE number this campaign
# has recorded — the 17.3751 baseline, the 43.6696 Perplexity bar, the 44.3995
# composite, every per-task delta — is ONE DRAW from a temperature-0.7
# process.
#
# Measured, and this is why the pin exists: task 56's IDENTICAL article,
# re-judged, scored **46.2359** against its recorded **43.1843**. A +3.05
# swing on unchanged input, against a margin-of-interest of +0.52.
#
# The official protocol scores 100 tasks, so per-call noise averages out. We
# score 10, where it dominates. Since the local 27B is already a declared
# substitution for the official gemini/GPT-5.5-class judge, determinism is
# worth more here than fidelity to that judge's sampler.
#
# Consequence, and it is not optional: the Perplexity bar and our own arm are
# BOTH re-measured under this pin before any comparison is made. A pinned
# reading cannot be compared against an unpinned one.
JUDGE_TEMPERATURE = 0.0
JUDGE_TOP_P = 1.0


def pin_sampling(client):
    """Force greedy decoding on the vendored client without editing the
    pinned clone. `_post` is the single place every judge payload passes
    through, so wrapping it is the one decider (§10.6) — adding the
    parameters at each call site would let two paths disagree."""
    original_post = client._post

    def post(payload):
        return original_post(dict(payload,
                                  temperature=JUDGE_TEMPERATURE,
                                  top_p=JUDGE_TOP_P))

    client._post = post
    return client


def load(path):
    return [json.loads(l) for l in open(path) if l.strip()]


def criteria_for(task):
    for r in load(f'{CLONE}/data/criteria_data/criteria.jsonl'):
        if int(r['id']) == task:
            return r
    sys.exit(f'no criteria for task {task}')


def reference_for(task):
    for r in load(f'{CLONE}/data/test_data/raw_data/reference.jsonl'):
        if int(r['id']) == task:
            return r['article']
    sys.exit(f'no reference for task {task}')


def prompt_for(task):
    for r in load(f'{CLONE}/data/prompt_data/query.jsonl'):
        if int(r['id']) == task:
            return r['prompt']
    sys.exit(f'no prompt for task {task}')


def derive(task, judge_json):
    """The driver's exact arithmetic (deepresearch_bench_race.py:155-175)."""
    cd = criteria_for(task)
    s = calculate_weighted_scores(judge_json, cd)
    t, r = s['target']['total'], s['reference']['total']
    overall = t / (t + r) if (t + r) > 0 else 0
    dims = {}
    for d in DIMS:
        k = f'{d}_weighted_avg'
        td, rd = s['target']['dims'].get(k, 0), s['reference']['dims'].get(k, 0)
        dims[d] = td / (td + rd) if (td + rd) > 0 else 0
    return {'id': task, 'overall_score': overall, **dims,
            'target_total': t, 'reference_total': r}


def judge(task, article, model):
    """One judge call through the VENDORED client — identical payload to
    score_race.py (max_completion_tokens=64000, reasoning_effort=medium,
    temperature/top_p intentionally unset). A hand-rolled HTTP call
    diverged on sampling params and would not be comparable to the
    17.3751 baseline."""
    from utils.api import AIClient                                  # noqa: E402
    cd = criteria_for(task)
    user = generate_merged_score_prompt.format(
        task_prompt=prompt_for(task), article_1=article,
        article_2=reference_for(task), criteria_list=format_criteria_list(cd))
    client = pin_sampling(AIClient(model=model))
    if not client.base_url.startswith('http://127.0.0.1:9741'):
        sys.exit(
            'exit 2: judge guard — base_url is %r, not the local daemon. '
            'Refusing: a substituted judge is not the pinned instrument (18.3).'
            % client.base_url)
    served = json.load(urllib.request.urlopen(BASE + '/models', timeout=60))
    ids = {m.get('id') for m in served.get('data', [])}
    if model not in ids:
        sys.exit('exit 2: judge guard — %r is not served locally (have %d models)'
                 % (model, len(ids)))
    # The official driver checks only that each dimension KEY exists. A
    # key whose list is EMPTY passes that check and then scores 0/0
    # through calculate_weighted_scores — the dimension silently
    # contributes nothing to either side, and a could-not-judge is
    # recorded as a score. Measured on task 65: `readability` came back
    # `[]` and the run reported 46.56 with a whole dimension missing.
    # Four verdicts, not two (§18.1): this retries, then REFUSES.
    last = None
    for attempt in range(1, 4):
        t0 = time.time()
        raw = client.generate(user_prompt=user, system_prompt="")
        js = extract_json_from_markdown(raw)
        if not js:
            last = 'no JSON in judge response'
            print('  attempt %d: %s' % (attempt, last), file=sys.stderr)
            continue
        jj = json.loads(js)
        missing = [d for d in DIMS if d not in jj]
        empty = [d for d in DIMS if d in jj and not jj[d]]
        if missing or empty:
            last = 'missing dims %s, empty dims %s' % (missing, empty)
            print('  attempt %d: %s — retrying' % (attempt, last), file=sys.stderr)
            continue
        print('  judge %.0fs (attempt %d)' % (time.time() - t0, attempt), file=sys.stderr)
        return jj
    sys.exit('exit 2: task %d judge never returned a scorable verdict (%s). '
             'NEVER-RAN, not zero.' % (task, last))


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--task', type=int, required=True)
    ap.add_argument('--replay')
    ap.add_argument('--article')
    ap.add_argument('--model', default='Qwen3.8-27B-UD-Q6_K_XL')
    ap.add_argument('--save-judge')
    a = ap.parse_args()
    if a.replay:
        row = next(r for r in load(a.replay) if int(r['id']) == a.task)
        jj = row['judge_output']
    else:
        jj = judge(a.task, open(a.article).read(), a.model)
        if a.save_judge:
            with open(a.save_judge, 'a') as f:
                f.write(json.dumps({'id': a.task, 'judge_model': a.model,
                                    'judge_output': jj}) + '\n')
    rec = derive(a.task, jj)
    print(json.dumps(rec, indent=1))
    print('OVERALL x100 = %.4f' % (100 * rec['overall_score']))
