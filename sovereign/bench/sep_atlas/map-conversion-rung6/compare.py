#!/usr/bin/env python3
"""rung 6 verdict: per-question judge ratio for each arm vs the July baseline.

usage: compare.py [arm ...]   (default: armA armB; e.g. `compare.py armA armB armB2`; missing arms are skipped)
Columns: judge = synth.judge_fact_score.ratio (the headline); kw = fact_score.ratio
(keyword facts in the answer); rows = the walk row that fired per question, read from
<arm>.log in question order ("walking the map ... row=<kind> (<source>...)").
"""
import json, os, re, sys
HERE = os.path.dirname(os.path.abspath(__file__))
BASE = os.path.join(HERE, '../../sep/baselines/questions-synth/2026-07-06.json')
NAMED = ['argument_consequence_against_compatibilism', 'dialectical_gettier_lottery',
         'position_summary_kripke_reference_causal_historical', 'comparative_berlin_liberty',
         'argument_aristotle_hylomorphism', 'contested_bioethics_principlism']

def load(p):
    if not os.path.exists(p):
        return None
    d = json.load(open(p))
    return {r['question_id']: r for r in d['results']}

def rows_from_log(p):
    """Question-ordered list of (row, source) — one per deep turn; '-' when the walk did not run."""
    if not os.path.exists(p):
        return []
    out, cur = [], None
    for raw in open(p, errors='replace'):
        line = re.sub(r'\x1b\[[0-9;]*m', '', raw)  # the log is ANSI-coloured
        m = re.search(r'walking the map .*?row=(\w+) \((\w+)', line)
        if m:
            cur = (m.group(1), m.group(2))
        if 'deep_turn_summary' in line:
            out.append(cur or ('-', '-'))
            cur = None
    return out

def ratio(r, *path):
    x = r
    for k in path:
        x = (x or {}).get(k) if isinstance(x, dict) else None
    return None if x is None else float(x)

ARMS = sys.argv[1:] or ['armA', 'armB']
arms = {'jul': load(BASE)}
for name in ARMS:
    arms[name] = load(os.path.join(HERE, name + '.json'))
rows = {name: rows_from_log(os.path.join(HERE, name + '.log')) for name in ARMS}
present = [n for n, a in arms.items() if a]
order = list(arms['jul'].keys())

short = lambda n: n.replace('arm', '')
hdr = f"{'question':<52}" + ''.join(f"{short(n)+'.judge':>10}{short(n)+'.kw':>8}" for n in present) + ''.join(f"{short(n)+'.row':>14}" for n in ARMS)
print(hdr); print('-' * len(hdr))
sums = {n: [0.0, 0.0, 0] for n in present}
for i, q in enumerate(order):
    line = f"{('* ' if q in NAMED else '  ') + q:<52}"
    for n in present:
        r = arms[n].get(q)
        j = ratio(r, 'synth', 'judge_fact_score', 'ratio'); k = ratio(r, 'fact_score', 'ratio')
        line += f"{(f'{j:.2f}' if j is not None else '—'):>10}{(f'{k:.2f}' if k is not None else '—'):>8}"
        if j is not None:
            sums[n][0] += j; sums[n][1] += (k or 0); sums[n][2] += 1
    for n in ARMS:
        rw = rows[n][i] if i < len(rows[n]) else ('—', '')
        line += f"{(rw[0] + ('' if rw[1] in ('', '-') else '/' + rw[1][:4])):>14}"
    print(line)
print('-' * len(hdr))
line = f"{'mean (n)':<52}"
for n in present:
    s = sums[n]
    line += f"{(s[0]/s[2] if s[2] else 0):>10.3f}{(s[1]/s[2] if s[2] else 0):>8.3f}" + ('' if s[2] == 21 else f" n={s[2]}")
print(line)
for n in ARMS:
    fired = sum(1 for r in rows[n] if r[0] not in ('-', 'unfiltered'))
    if rows[n]:
        print(f"{n}: rows fired on {fired}/{len(rows[n])} questions walked")
print("* = the six questions named before the run. Bars: B.judge mean >= 0.916; B >= A on the six.")
