"""twins.py: each world gold deal's individuation risk from gold alone (feature twins, features changing, rollovers), against how often it is missed."""
import json, os, re, glob, collections, sys
sys.path.insert(0, 'research/ontology-apps/loop')
from misses import served
READS = {'world-s1': ['fit-inner', 'w20-inner', 'beside-s1', 'structural-fresh-s1', 'bundle1-fresh'], 'world-s2': ['structural-fresh', 'bundle1-fresh', 'guard-replay'],
         'world-s3': ['fit-inner', 'voice-inner', 'voice2-s3', 'w20-inner', 'structural-fresh', 'bundle1-fresh'], 'world-s4': ['structural-fresh', 'bundle1-fresh', 'guard-replay'],
         'world-s5': ['structural-fresh', 'bundle1-fresh', 'guard-replay']}
def parse(desc):
    m = re.match(r'(?P<product>[^,]+), (?P<term>.+), (?P<vol>[^,]+?) at (?P<point>[^;]+)', desc or '')
    return {k: v.strip().lower() for k, v in m.groupdict().items()} if m else None
rows = []
for cid, arms in READS.items():
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    tags = collections.defaultdict(list)
    for t in map(json.loads, open(f'{base}/tags.jsonl')): tags[t['deal']].append(t)
    deals = {}
    for f in glob.glob(f'{base}/gold/*.json'):
        g = json.load(open(f))
        for d in g['deals']: deals[d['id']] = dict(d, folder=g['folder'], p=parse(d['description']))
    scores = [json.load(open(f'runs/loop/{a}/{cid}/score.json')) for a in arms if os.path.exists(f'runs/loop/{a}/{cid}/score.json')]
    for g, d in deals.items():
        if not scores or g not in scores[0]['gold']: continue
        sib = [o for o in deals.values() if o['folder'] == d['folder'] and o['id'] != g]
        p = d['p']
        twin = bool(p) and any(o['p'] and o['p']['product'] == p['product'] and o['p']['point'] == p['point'] for o in sib)
        master = d.get('kind') == 'master_agreement'
        changes = any(t.get(k) for t in tags[g] for k in ('term_change', 'renegotiation', 'correction'))
        roll = 'rolls over' in (d['description'] or '') or any('rolls over ' + g in (o['description'] or '') for o in sib)
        revive = any(t.get('revive') or t.get('cancel') for t in tags[g])
        missed = sum(not served(S, g) for S in scores)
        rows.append(dict(cid=cid, g=g, twin=twin, master=master, changes=changes, roll=roll, revive=revive, reads=len(scores), missed=missed))
def show(name, key):
    t = collections.defaultdict(lambda: [0, 0])
    for r in rows: t[key(r)][0] += r['missed']; t[key(r)][1] += r['reads']
    print(name, {k: f"{v[0]}/{v[1]} missed ({v[0]/v[1]:.0%})" for k, v in sorted(t.items(), key=str)})
show('feature twin (same product and point, another deal of the counterparty):', lambda r: r['twin'])
show('master agreement:', lambda r: r['master'])
show('features change in life (term change, renegotiation, correction):', lambda r: r['changes'])
show('rollover (rolls over / rolled over by):', lambda r: r['roll'])
show('cancel or revive:', lambda r: r['revive'])
show('any individuation risk (twin, roll, revive, changes):', lambda r: r['twin'] or r['roll'] or r['revive'] or r['changes'])
