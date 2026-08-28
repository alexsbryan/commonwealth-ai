#!/usr/bin/env python3
"""THE RULER'S OWN SPREAD — judge ONE unchanged article k times and report it.

WHY THIS EXISTS. Every A/B this campaign has run attributes its spread to the
PIPELINE. That attribution has never been tested. 15 fingerprinted articles are
on disk (drb/overall-derivation/flights-*/*.judge.jsonl) and every one was
judged exactly ONCE, so the judge's own contribution to the 7.56-point spread
(notes 86ac6f7c, e2807981) is unmeasured. If the ruler owns most of it, every
lever verdict taken to date is uninterpretable and reps -- not cleverness --
are the only fix.

The judge is pinned greedy (judge_instrument.py: temperature 0.0, top_p 1.0),
so the NAIVE expectation is zero spread. It is not guaranteed: llama.cpp
batching, slot reuse and KV state can move logits between calls, and the
daemon is restarted mid-series when RSS approaches the OOM band. This measures
what the instrument ACTUALLY does under the conditions the arms ran under.

  ruler_noise.py --task 69 --article <abs> --reps 5 [--out FILE] [--allow-busy]

REFUSES to run while another tenant holds the daemon (note 160268d0: the
daemon SHEDS at >30s predicted wait rather than queueing, so a contended judge
call fails silently and reads as a low score, not as an error).
Four verdicts, not two: an unscorable draw is reported MISSING, never zero.
"""
import argparse, json, os, pathlib, re, subprocess, sys, time, hashlib, statistics as st

HERE = pathlib.Path(__file__).resolve().parent
DERIV = HERE.parents[1] / 'drb' / 'overall-derivation'
BASE = 'http://127.0.0.1:9741/v1'


def other_tenants():
    """Named tenants that would starve a judge call. Never a bare process-name
    pgrep: that pattern matches the guarding shell itself (note 160268d0 --
    a prior session lost 45 minutes to exactly this, and a `pkill -f` killed
    its own shell). Match on the absolute binary path and exclude self."""
    me = {str(os.getpid()), str(os.getppid())}
    out = []
    try:
        ps = subprocess.run(['ps', '-eo', 'pid=,args='], capture_output=True,
                            text=True, timeout=30).stdout
    except Exception as e:
        return [f'could not enumerate processes ({e}) -- refusing to assume idle']
    for line in ps.splitlines():
        line = line.strip()
        if not line:
            continue
        pid, _, args = line.partition(' ')
        if pid in me:
            continue
        if '/target/debug/sovereign-cli deep-research' in args:
            out.append(f'pid {pid}: a deep-research FLIGHT is in progress')
        elif 'sovereign session distill' in args or 'session-frame.sh' in args:
            out.append(f'pid {pid}: a session-frame DISTILL is running (~8 min, 27B)')
        elif 'score_one.py' in args or 'score_race.py' in args:
            out.append(f'pid {pid}: another RACE scorer is running')
    return out


def daemon_rss_gb():
    try:
        pid = subprocess.run(['pgrep', '-f', 'debug/sovereign-cli-daemon daemon run'],
                             capture_output=True, text=True, timeout=20).stdout.split()
        if not pid:
            return None
        rss = subprocess.run(['ps', '-o', 'rss=', '-p', pid[0]],
                             capture_output=True, text=True, timeout=20).stdout.strip()
        return int(rss) / 1048576 if rss else None
    except Exception:
        return None


def score_once(task, article_abs, save_judge):
    """One draw through score_one.py -- the SAME instrument the arms used
    (§10.6: one decider). Returns (record_or_None, reason_or_None, secs)."""
    env = dict(os.environ, LLM_BACKEND='openai', OPENAI_BASE_URL=BASE,
               OPENAI_API_KEY='local')
    t0 = time.time()
    p = subprocess.run([sys.executable, str(HERE / 'score_one.py'),
                        '--task', str(task), '--article', article_abs,
                        '--save-judge', save_judge],
                       capture_output=True, text=True, cwd=str(DERIV), env=env)
    secs = time.time() - t0
    m = re.search(r'\{\s*"id".*?\n\}', p.stdout, re.S)
    if not m:
        tail = (p.stderr or p.stdout).strip().splitlines()
        return None, (tail[-1] if tail else f'exit {p.returncode}, no record'), secs
    try:
        return json.loads(m.group(0)), None, secs
    except Exception as e:
        return None, f'unparseable record ({e})', secs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--task', type=int, required=True)
    ap.add_argument('--article', required=True)
    ap.add_argument('--reps', type=int, default=5)
    ap.add_argument('--out', default=None)
    ap.add_argument('--allow-busy', action='store_true',
                    help='run even though another tenant holds the daemon '
                         '(the result is then NOT a clean instrument read)')
    a = ap.parse_args()

    art = pathlib.Path(a.article).resolve()
    if not art.exists():
        print(f'no such article {art}', file=sys.stderr)
        sys.exit(2)
    body = art.read_bytes()
    sha = hashlib.sha256(body).hexdigest()

    busy = other_tenants()
    if busy:
        print('DAEMON HAS OTHER TENANTS:', file=sys.stderr)
        for b in busy:
            print('  ' + b, file=sys.stderr)
        if not a.allow_busy:
            print('REFUSING. The daemon sheds rather than queues (note '
                  '160268d0); a contended draw is not an instrument read. '
                  'Wait, or pass --allow-busy and label the result.',
                  file=sys.stderr)
            sys.exit(3)
        print('  --allow-busy given: proceeding, result is LABELLED CONTENDED',
              file=sys.stderr)

    out = a.out or str(DERIV / 'flights-ruler-noise' / f't{a.task}-ruler.json')
    pathlib.Path(out).parent.mkdir(parents=True, exist_ok=True)
    judge_sidecar = str(pathlib.Path(out).with_suffix('.judge.jsonl'))

    print(f'=== RULER NOISE  task {a.task}  reps {a.reps} ===')
    print(f'    article {art}')
    print(f'    sha256  {sha}   {len(body)} bytes')
    print(f'    judge pinned greedy (temperature 0.0, top_p 1.0)')
    print(f'    contended: {"YES -- " + "; ".join(busy) if busy else "no"}')

    draws, missing = [], []
    for i in range(1, a.reps + 1):
        rss = daemon_rss_gb()
        rec, why, secs = score_once(a.task, str(art), judge_sidecar)
        if rec is None:
            missing.append({'draw': i, 'reason': why, 'secs': round(secs, 1),
                            'daemon_rss_gb': rss})
            print(f'  draw {i}: MISSING -- {why}  ({secs:.0f}s, daemon '
                  f'{rss:.1f}G)' if rss else f'  draw {i}: MISSING -- {why}')
            continue
        rec['_draw'] = i
        rec['_secs'] = round(secs, 1)
        rec['_daemon_rss_gb'] = rss
        draws.append(rec)
        print(f'  draw {i}: overall {100*rec["overall_score"]:7.4f}   '
              f'({secs:.0f}s, daemon {rss:.1f}G)' if rss else
              f'  draw {i}: overall {100*rec["overall_score"]:7.4f}  ({secs:.0f}s)')

    DIMS = ['comprehensiveness', 'insight', 'instruction_following', 'readability']
    res = {'task': a.task, 'article': str(art), 'article_sha256': sha,
           'article_bytes': len(body), 'reps_requested': a.reps,
           'contended': busy, 'temperature': 0.0, 'top_p': 1.0,
           'draws': draws, 'missing': missing}

    print()
    if len(draws) < 2:
        print('RESOLVED NOTHING: fewer than 2 scorable draws '
              f'({len(draws)} of {a.reps}). Missing cells are missing, not zero.')
        res['verdict'] = 'could-not-judge'
    else:
        v = [100 * d['overall_score'] for d in draws]
        spread = max(v) - min(v)
        sd = st.stdev(v)
        res['overall'] = {'n': len(v), 'mean': st.mean(v), 'spread': spread,
                          'sd': sd, 'min': min(v), 'max': max(v), 'values': v}
        print(f'OVERALL  n={len(v)}  mean {st.mean(v):.4f}  '
              f'SPREAD {spread:.4f}  sd {sd:.4f}   [{min(v):.4f} .. {max(v):.4f}]')
        for d in DIMS:
            dv = [100 * x[d] for x in draws]
            res.setdefault('dims', {})[d] = {
                'mean': st.mean(dv), 'spread': max(dv) - min(dv), 'values': dv}
            print(f'   {d:22s} mean {st.mean(dv):7.4f}  spread {max(dv)-min(dv):7.4f}')
        # The bars, pre-registered in frame b95e5e21 step 1 BEFORE this data existed.
        if spread >= 4:
            res['verdict'] = 'ruler-dominates'
            print(f'\nBAR (pre-registered): spread >= 4 on IDENTICAL input.')
            print(f'  OBSERVED {spread:.2f} -- THE RULER DOMINATES. Every lever A/B to '
                  'date is uninterpretable.\n  The fix (average k judge calls per '
                  'article, and/or pin judge sampling harder) lands BEFORE any further arm.')
        elif spread < 2:
            res['verdict'] = 'pipeline-owns-it'
            print(f'\nBAR (pre-registered): spread < 2 on identical input.')
            print(f'  OBSERVED {spread:.2f} -- the PIPELINE owns the 7.56-point spread. '
                  'Only reps help; the ruler is sound.')
        else:
            res['verdict'] = 'both-live'
            print(f'\nBAR (pre-registered): 2 <= spread < 4.')
            print(f'  OBSERVED {spread:.2f} -- report the band; treat ruler and '
                  'pipeline as BOTH live contributors.')
        if missing:
            print(f'  CAVEAT: {len(missing)} of {a.reps} draws were unscorable and '
                  'are excluded as MISSING, never as zero.')

    pathlib.Path(out).write_text(json.dumps(res, indent=2))
    print(f'\n-> {out}')
    print(f'-> {judge_sidecar}  (per-draw judge output + article fingerprint)')


if __name__ == '__main__':
    main()
