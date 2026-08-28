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
import hashlib, json, sys, os, argparse, pathlib, time, urllib.request

# The clone's driver imports `utils.api` transitively, and that module freezes
# LLM_BACKEND / base_url / api_key / socket timeout at IMPORT time — so all of
# it must be decided ABOVE that import. Both facts were learned here the hard
# way (a first judge call that left the host carrying a real key; a task-83 run
# cut off client-side at 600s and recorded NEVER-RAN), and both fixes lived in
# THIS file only, while score_race.py had neither. They are instrument state,
# so they now live in judge_instrument beside the sampling pin — one decider
# for how a judge call is made (§10.6).
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2]))
from judge_instrument import configure_judge_client                # noqa: E402

BASE = configure_judge_client()

CLONE = '/home/alexbryan/dev/deep_research_bench'
sys.path.insert(0, CLONE)
from deepresearch_bench_race import format_criteria_list          # noqa: E402
from prompt.score_prompt_en import generate_merged_score_prompt   # noqa: E402
from utils.score_calculator import calculate_weighted_scores      # noqa: E402
from utils.json_extractor import extract_json_from_markdown       # noqa: E402

# The judge instrument — sampling pin + the scorable-verdict predicate —
# is ONE decider shared with drb/overall-derivation/score_race.py (§10.6).
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2]))
from judge_instrument import (DIMS, JUDGE_TEMPERATURE, JUDGE_TOP_P,   # noqa: E402
                             pin_sampling, unscorable)


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
        last = unscorable(jj)
        if last:
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
        article = open(a.article).read()
        jj = judge(a.task, article, a.model)
        if a.save_judge:
            # Fingerprint WHAT was judged. Without it a stored sidecar proves
            # only that some article scored X — a later replay cannot tell a
            # re-judge of the same bytes from a judge of different ones, so
            # neither a determinism check nor an A/B built on it is decidable
            # (§11.1: cite, don't recall).
            with open(a.save_judge, 'a') as f:
                f.write(json.dumps({
                    'id': a.task, 'judge_model': a.model,
                    'article_path': str(pathlib.Path(a.article).resolve()),
                    'article_sha256': hashlib.sha256(
                        article.encode('utf-8')).hexdigest(),
                    'article_chars': len(article),
                    'temperature': JUDGE_TEMPERATURE, 'top_p': JUDGE_TOP_P,
                    'judge_output': jj}) + '\n')
    rec = derive(a.task, jj)
    print(json.dumps(rec, indent=1))
    print('OVERALL x100 = %.4f' % (100 * rec['overall_score']))
