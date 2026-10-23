"""single_point.py <arm>/<corpus> ...: per deal, how many of its gold updates were read right, and for missed deals how many went wrong."""
import sys, json, os, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
REL = ["superseded", "cancel", "revive", "renegotiation", "several_deals_in_message", "rename"]
per = []
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    D = M.load(f'runs/loop/{arm}/{cid}', cid); S = D['S']
    tp = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}/tags.jsonl')
    tags = collections.defaultdict(list)
    if os.path.exists(tp):
        for t in map(json.loads, open(tp)):
            if t['kind'] == 'stage_update': tags[t['deal']].append(t)
    for g in S['gold']:
        ups = [u for u in D['gold'] if u['deal'] == g]
        recs = set(S['gold'][g]['records'])
        right = 0
        for u in ups:
            c = M.chain(D, u)
            right += c['status'] == 'emitted' and bool(set(c['records']) & recs)
        rel = any(t.get(k) for t in tags[g] for k in REL) if tags else None
        per.append(dict(corpus=cid, deal=g, n=len(ups), right=right, served=M.served(S, g), rel=rel))
for lab, sel in [('worlds', lambda p: p['corpus'] != 'ward-story'), ('ward', lambda p: p['corpus'] == 'ward-story')]:
    ps = [p for p in per if sel(p)]
    if not ps: continue
    m = [p for p in ps if not p['served']]
    wrong = collections.Counter(min(p['n'] - p['right'], 3) for p in m)
    print(f"{lab}: {len(ps)} deals, {len(m)} missed; missed deals by gold updates read wrong: {dict(sorted(wrong.items()))} (3 = 3+)")
    print(f"   missed deals: mean updates {sum(p['n'] for p in m)/max(1,len(m)):.1f}; served deals: {sum(p['n'] for p in ps if p['served'])/max(1,len(ps)-len(m)):.1f}")
    for r in (True, False):
        q = [p for p in ps if p['rel'] is r]
        if q: print(f"   {'relational' if r else 'plain':<10} deals {len(q):>3}  mean updates {sum(p['n'] for p in q)/len(q):.1f}  missed {sum(not p['served'] for p in q)}")
