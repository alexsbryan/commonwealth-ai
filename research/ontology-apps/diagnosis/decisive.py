"""decisive.py <arm>/<corpus> ...: for every gold deal's record, the reading its stage fold selected, classed."""
import sys, json, re, collections
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
cls_all = collections.Counter(); cls_miss = collections.Counter(); ex = collections.defaultdict(list)
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    run = f'runs/loop/{arm}/{cid}'
    D = M.load(run, cid); S = D['S']
    atoms = json.load(open(f'{run}/atlas/atoms.json')); atoms = atoms if isinstance(atoms, list) else atoms['atoms']
    by_id = {a['data'].get('id'): a['data'] for a in atoms}
    for g in sorted(S['gold']):
        ok = M.served(S, g)
        ups = sorted([u for u in D['gold'] if u['deal'] == g], key=lambda u: u['dt'])
        last_i = ups[-1]['i']
        recs = S['gold'][g]['records']
        if not recs:
            k = 'no record'; cls_all[k] += 1; cls_miss[k] += not ok; ex[k].append((spec, g)); continue
        # the record holding the deal's last gold update, else the first
        holding = [r for r in recs if any(last_i in S['matches'].get(c, []) for c in S['placed'].get(r, []))]
        r = (holding or recs)[0]
        f = (by_id.get(r, {}).get('attributes') or {}).get('__folds', {}).get('stage', {})
        sel = [rd for rd in f.get('readings', []) if rd.get('disposition') == 'selected']
        hits = S['matches'].get(sel[0]['claim'], []) if sel else []
        deals_hit = {D['gold'][h]['deal'] for h in hits}
        if not sel: k = 'no stage selected'
        elif last_i in hits: k = 'selected = its last update' + ('' if sel[0]['value'] == ups[-1]['stage'] else ', stage misread')
        elif g in deals_hit: k = 'selected = an earlier update of it'
        elif deals_hit: k = "selected = another deal's update"
        else: k = 'selected = a statement gold does not file'
        if len(recs) > 1: k += ' [split]'
        if not holding: k += ' [last update in no record]'
        cls_all[k] += 1; cls_miss[k] += not ok
        if not ok: ex[k].append((spec.split('/')[1], g, (sel[0]['evidence'][:70] if sel else '')))
print(f"{'class':<64} deals missed")
for k in sorted(cls_all, key=lambda k: -cls_miss[k]): print(f"{k:<64} {cls_all[k]:>5} {cls_miss[k]:>6}")
for k, v in ex.items():
    if cls_miss[k]: print('\n', k); [print('    ', x) for x in v if len(x) == 3 or True]
