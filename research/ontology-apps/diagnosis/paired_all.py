"""paired_all.py: each gold update (and nomination) read at 40k and at 20k: history of its deal in its window, and right or not, paired."""
import sys, json, os, re, glob, collections
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
pairs = collections.Counter(); dose = collections.defaultdict(collections.Counter)
for cid, a40, a20 in [('world-s1', 'structural-fresh-s1', 'beside-s1'), ('world-s1', 'bundle1-fresh', 'w20-inner'), ('world-s3', 'structural-fresh', 'w20-inner'), ('world-s3', 'bundle1-fresh', 'w20-inner')]:
    A, B = M.load(f'runs/loop/{a40}/{cid}', cid), M.load(f'runs/loop/{a20}/{cid}', cid)
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    doc_of = {k: M.slug(v) for k, v in json.load(open(f'{base}/msgids.json')).items() if v}
    date_of = {}
    for p in glob.glob(f'{base}/story/*/*'):
        m = re.search(r'^Date:(.*)$', open(p, errors='replace').read(), re.M)
        if m: date_of[doc_of.get('/'.join(p.split('/')[-2:]))] = P(re.sub(r"\s*\(.*\)$", "", m.group(1).strip()))
    def history(D, deal, doc):
        d = D['deals'][deal]; ch = D['where'].get(doc, (None,))[0]
        msgs = [doc_of.get(f"{d['folder']}/{m}") for m in d.get('messages', [])]
        return sum(1 for x in msgs if x and x != doc and date_of.get(x) and date_of.get(doc) and date_of[x] < date_of[doc] and D['where'].get(x, (None,))[0] == ch)
    def right(D, u):
        c = M.chain(D, u); return c['status'] == 'emitted' and bool(set(c['records']) & set(D['S']['gold'][u['deal']]['records']))
    for ua in A['gold']:
        ub = next(x for x in B['gold'] if x['i'] == ua['i'])
        ha, hb = history(A, ua['deal'], ua['at']), history(B, ub['deal'], ub['at'])
        ra, rb = right(A, ua), right(B, ub)
        for h, r in ((ha, ra), (hb, rb)): dose[min(h, 3)][r] += 1
        if (ha > 0) != (hb > 0):
            with_h, without_h = (ra, rb) if ha > 0 else (rb, ra)
            pairs[('history in view: right' if with_h else 'history in view: WRONG', 'no history: right' if without_h else 'no history: WRONG')] += 1
print('pairs where only one read had the deal\'s earlier messages in the window:')
for k, n in sorted(pairs.items()): print(f"   {k[0]:<28} {k[1]:<22} {n:>3}")
print('all reads, by how many earlier messages of the deal shared the window:')
for h in sorted(dose): c = dose[h]; n = sum(c.values()); print(f"   {h if h < 3 else '3+'}: {n:>4} updates, wrong {c[False]/n:.0%}")
