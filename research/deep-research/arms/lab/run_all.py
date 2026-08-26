#!/usr/bin/env python3
"""End-to-end lab run: compose -> verify -> score -> composite.

Serial by construction: the daemon serves ONE request at a time (it answers
503 `local_queue_full` when busy), so there is no parallelism to exploit and
overlapping stages only produce retries.

The composite is the same arithmetic the benchmark uses:
  per task   overall_i = T_i / (T_i + R_i)
  subset     RACE      = 100 * mean_i(overall_i)
R_i is the reference article's weighted score and is FIXED per task, so the
target is known in advance: every task needs T ~= 6.3 against R ~= 9.3 to
reach the 40.46 the run must beat.
"""
import json, os, sys, time, argparse, statistics, subprocess, logging

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
logging.disable(logging.WARNING)
SUBSET = [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--tasks', default=','.join(str(t) for t in SUBSET))
    ap.add_argument('--model', default='Qwen3.8-27B-UD-Q6_K_XL')
    ap.add_argument('--judge', default='Qwen3.8-27B-UD-Q6_K_XL')
    ap.add_argument('--out', default=os.path.join(HERE, 'out/v2'))
    ap.add_argument('--stage', default='all', choices=['compose', 'score', 'verify', 'all'])
    # Writer knobs, passed through to compose2 so an arm can be varied from
    # the command line instead of by editing the composer in place. That edit
    # is why the v2 arm is unreproducible: its word budget was changed on disk
    # before v3 ran and nothing recorded the old value. Defaults here are
    # deliberately `None` = "whatever compose2's signature says", so this
    # wrapper never becomes a second place that decides the arm (§10.6).
    ap.add_argument('--k', type=int, default=None)
    ap.add_argument('--repeat-cap', type=int, default=None)
    ap.add_argument('--words', default=None,
                    help='section word budget as "min,max" (e.g. 300,380)')
    a = ap.parse_args()
    tasks = [int(x) for x in a.tasks.split(',')]
    os.makedirs(a.out, exist_ok=True)

    if a.stage in ('compose', 'all'):
        from compose2 import compose2
        for t in tasks:
            rp = f'{a.out}/drb-{t}/dr-lab/report.md'
            if os.path.exists(rp):
                print(f'  task {t}: report exists, skipping compose', flush=True)
                continue
            t0 = time.time()
            kw = {}
            if a.k is not None:
                kw['k'] = a.k
            if a.repeat_cap is not None:
                kw['repeat_cap'] = a.repeat_cap
            if a.words is not None:
                kw['words'] = tuple(int(x) for x in a.words.split(','))
            compose2(t, a.model, a.out, **kw)
            print(f'  task {t} composed in {time.time()-t0:.0f}s', flush=True)

    if a.stage in ('score', 'all'):
        from score_one import judge, derive
        recs_path = f'{a.out}/records.jsonl'
        done = {}
        if os.path.exists(recs_path):
            for l in open(recs_path):
                r = json.loads(l)
                done[r['id']] = r
        for t in tasks:
            if t in done:
                print(f'  task {t}: scored already ({100*done[t]["overall_score"]:.2f})', flush=True)
                continue
            rp = f'{a.out}/drb-{t}/dr-lab/report.md'
            if not os.path.exists(rp):
                print(f'  task {t}: NO REPORT — skipped (never-ran, not zero)', flush=True)
                continue
            t0 = time.time()
            try:
                jj = judge(t, open(rp).read(), a.judge)
            except SystemExit as e:
                print('  task %d: NEVER-RAN (%s)' % (t, e), flush=True)
                continue
            except Exception as e:
                print('  task %d: NEVER-RAN (%s)' % (t, e), flush=True)
                continue
            with open(f'{a.out}/judge.jsonl', 'a') as f:
                f.write(json.dumps({'id': t, 'judge_model': a.judge, 'judge_output': jj}) + '\n')
            rec = derive(t, jj)
            rec['seconds'] = round(time.time() - t0)
            rec['words'] = len(open(rp).read().split())
            with open(recs_path, 'a') as f:
                f.write(json.dumps(rec) + '\n')
            done[t] = rec
            print(f'  task {t}: T={rec["target_total"]:.2f} R={rec["reference_total"]:.2f} '
                  f'overall={100*rec["overall_score"]:.2f} ({rec["seconds"]}s, {rec["words"]}w)', flush=True)

        recs = [done[t] for t in tasks if t in done]
        if recs:
            print()
            print('%-6s %8s %8s %9s %8s' % ('TASK', 'T', 'R', 'OVERALL', 'WORDS'))
            for r in recs:
                print('%-6d %8.2f %8.2f %9.2f %8d'
                      % (r['id'], r['target_total'], r['reference_total'],
                         100 * r['overall_score'], r.get('words', 0)))
            print()
            for d in ['comprehensiveness', 'insight', 'instruction_following', 'readability']:
                print('  %-22s %.4f' % (d, 100 * statistics.mean(r[d] for r in recs)))
            comp = 100 * statistics.mean(r['overall_score'] for r in recs)
            print('  %-22s %.4f   (n=%d of %d)' % ('COMPOSITE', comp, len(recs), len(tasks)))
            print()
            print('  Perplexity official 40.46 | 27B judge reads Perplexity at 43.67')
            print('  baseline t7a graded-probe 17.3751')


main()
