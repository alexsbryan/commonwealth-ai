"""deal_signs.py <arm>/<corpus> ...: whole-record signs computed without gold, then scored against gold."""
import sys, json, itertools, collections
sys.path.insert(0, 'research/ontology-apps/loop')
from misses import served
PRE = {'lead', 'proposal', 'negotiating'}
tot = collections.Counter(); caught = collections.defaultdict(set); missed_all = set()
for spec in sys.argv[1:]:
    S = json.load(open(f'runs/loop/{spec}/score.json'))
    atoms = json.load(open(f'runs/loop/{spec}/atlas/atoms.json')); atoms = atoms if isinstance(atoms, list) else atoms['atoms']
    by = {a['data'].get('id'): a['data'] for a in atoms}
    recs = {d['id']: d for d in S['deals']}
    miss = {g for g in S['gold'] if not served(S, g)}; missed_all |= {(spec, g) for g in miss}
    def fold(r, f): return ((by.get(r, {}).get('attributes') or {}).get('__folds', {}) or {}).get(f, {})
    def vals(r, f):
        fo = fold(r, f); return {str(v).lower() for v in fo.get('values', []) or []} | {str(x.get('value')).lower() for x in fo.get('readings', []) if x.get('value')}
    for r, d in recs.items():
        rds = sorted(fold(r, 'stage').get('readings', []), key=lambda x: x.get('date') or '')
        gold_here = set(d['gold']); bad = bool(gold_here & miss)
        if len(rds) >= 2 and rds[-1]['value'] in PRE and rds[-2]['value'] in ('won', 'lost'):
            k = 'A1 pre-terminal after won' if rds[-2]['value'] == 'won' else 'A2 pre-terminal after lost'
            tot[(k, 'on a missed deal' if bad else ('on a served deal' if gold_here else 'on a record of no gold deal'))] += 1
            if bad: caught[k] |= {(spec, g) for g in gold_here & miss}
        for f in ('term', 'delivery_point'):
            if fold(r, f).get('outcome') == 'conflict':
                k = f'C {f} conflict'; merged = len(d['gold']) > 1 or bool(d['strays'])
                tot[(k, 'record holds two deals' if merged else ('on a missed deal' if bad else ('on a served deal' if gold_here else 'no gold')))] += 1
                if bad: caught[k] |= {(spec, g) for g in gold_here & miss}
    for a, b in itertools.combinations(recs, 2):
        ca, cb = vals(a, 'counterparty'), vals(b, 'counterparty')
        if not (ca and cb and ca & cb): continue
        ok = all(not (vals(a, f) and vals(b, f)) or (vals(a, f) & vals(b, f)) for f in ('delivery_point', 'term'))
        if not ok: continue
        ga, gb = set(recs[a]['gold']), set(recs[b]['gold'])
        if not ga and not gb: k2 = 'neither holds gold'
        elif ga & gb: k2 = 'one gold deal split (true)'
        else: k2 = 'different gold deals (false)'
        tot[('B same counterparty, point and term agree', k2)] += 1
        if ga & gb & miss: caught['B'] |= {(spec, g) for g in ga & gb & miss}
for k in sorted(tot): print(f"{k[0]:<44} {k[1]:<32} {tot[k]:>4}")
allc = set().union(*caught.values()) if caught else set()
print(f"\nmissed deals: {len(missed_all)}; flagged by any sign: {len(allc)}; by sign: {{k: len(v) for k, v in caught.items()}}")
