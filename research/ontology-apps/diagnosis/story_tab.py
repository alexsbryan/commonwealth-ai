"""story_tab.py <arm>/<world> ...: is a deal's last message read in a window holding an earlier message of the deal?"""
import sys, json, os, re, collections
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
tot = collections.defaultdict(lambda: [0, 0]); rows = []
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    D = M.load(f'runs/loop/{arm}/{cid}', cid); S = D['S']; c = D['c']
    doc_of = {k: M.slug(v) for k, v in json.load(open(c['msgids'])).items() if v}
    date_of = {}
    for f in c['folders']:
        g = json.load(open(f"{c['gold']}/{f}.json"))
    for g in S['gold']:
        deal = D['deals'][g]; fold = deal['folder']
        msgs = [doc_of.get(f"{fold}/{m}") for m in deal.get('messages', [])]
        placed = [(D['where'].get(m, (None,))[0], m) for m in msgs if m]
        # order the deal's messages by the window they were read in, then by their date in the prompt's own order
        wins = [w for w, _ in placed if w]
        if len(wins) < 2: k = 'one message read'
        else:
            last = max(wins); k = 'story with last' if wins.count(last) > 1 else 'last alone in its window'
        ok = M.served(S, g); tot[k][0] += ok; tot[k][1] += 1; rows.append((spec, g, k, ok))
for k, (s, n) in sorted(tot.items()): print(f"   {k:<26} missed {n-s:>2}/{n:<3} ({(n-s)/n:.0%})")
for r in rows:
    if not r[3]: print('     miss', r[:3])
