"""self_desc.py: every gold update in the fresh world reads, by how many of its deal's features its sentence states, against identity and placement."""
import sys, json, os, re, glob, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
def parse(desc):
    m = re.match(r'(?P<product>[^,]+), (?P<term>.+), (?P<vol>[^,]+?) at (?P<point>[^;]+)', desc or '')
    return {k: v.strip().lower() for k, v in m.groupdict().items()} if m else None
STOP = {'a', 'an', 'of', 'on', 'the', 'gas', 'at', 'from', 'to', 'for', 'per', 'day', 'mmbtu', 'd', 'years', 'year', 'option', 'service', 'supply'}
def toks(s): return {w for w in re.findall(r'[a-z0-9]+', s.lower()) if w not in STOP and len(w) > 1}
def stated(feature_text, sentence):
    t = toks(feature_text); return bool(t) and len(t & toks(sentence)) / len(t) >= 0.5
tab = collections.defaultdict(collections.Counter); pos = collections.defaultdict(list)
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    deals = {}
    for f in glob.glob(f'{base}/gold/*.json'):
        g = json.load(open(f))
        for d in g['deals']: deals[d['id']] = parse(d['description'])
    for p in glob.glob(f'runs/loop/*/{cid}/score.json'):
        d = os.path.dirname(p); rr = json.load(open(f'{d}/run.json'))
        if rr.get('asker') not in ('daemon', 'recorded') or d.split('/')[2].startswith(('det-', 'carried', 'marked', 'budget', 'header')): continue
        if not os.path.exists(f'{d}/extract.err'): continue
        try: D = M.load(d, cid)
        except Exception: continue
        S = D['S']
        rec_deals = collections.defaultdict(collections.Counter)
        for r, cids in S['placed'].items():
            for c in cids:
                for h in S['matches'].get(c, []): rec_deals[r][D['gold'][h]['deal']] += 1
        main = {g: max(((n[g], r) for r, n in rec_deals.items() if n[g]), default=(0, None))[1] for g in S['gold']}
        owner = {r: n.most_common(1)[0][0] for r, n in rec_deals.items() if n}
        by = collections.defaultdict(list)
        for u in D['gold']: by[u['deal']].append(u)
        for g, ups in by.items():
            ups.sort(key=lambda u: u['dt'])
            f = deals.get(g)
            if not f: continue
            for i, u in enumerate(ups):
                n = sum(stated(f[k], u['quote']) for k in ('product', 'vol', 'point', 'term'))
                cids = [c for r, cs in S['placed'].items() for c in cs if u['i'] in S['matches'].get(c, [])]
                recs = {r for r, cs in S['placed'].items() for c in cs if c in cids}
                out = 'not placed' if not cids else ('identity wrong' if not (main[g] in recs and owner.get(main[g]) == g) else 'right')
                tab[n][out] += 1
                pos['first half' if i < len(ups) / 2 else 'second half'].append(n)
for n in sorted(tab):
    c = tab[n]; t = sum(c.values())
    print(f"features stated {n}: {t:>5} updates  identity wrong {c['identity wrong']/t:.1%}  not placed {c['not placed']/t:.1%}")
for k, v in pos.items(): print(f"{k}: mean features stated {sum(v)/len(v):.2f}  stating none {sum(1 for x in v if x == 0)/len(v):.0%}")
