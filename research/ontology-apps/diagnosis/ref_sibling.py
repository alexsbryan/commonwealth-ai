"""ref_sibling.py: every gold update in the fresh world reads, its identity outcome, by reference form x close siblings (and rename)."""
import sys, json, os, re, glob, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
def parse(desc):
    m = re.match(r'(?P<product>[^,]+), (?P<term>.+), (?P<vol>[^,]+?) at (?P<point>[^;]+)', desc or '')
    return {k: v.strip().lower() for k, v in m.groupdict().items()} if m else None
tab = collections.defaultdict(collections.Counter)
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    tags = {(t['folder'], t['file'], t['deal']): t for t in map(json.loads, open(f'{base}/tags.jsonl')) if t['kind'] == 'stage_update'}
    deals = {}
    for f in glob.glob(f'{base}/gold/*.json'):
        g = json.load(open(f))
        for d in g['deals']: deals[d['id']] = dict(d, folder=g['folder'], p=parse(d['description']))
    def close(g):
        d = deals[g]; p = d['p']
        if not p: return False
        return any(o['folder'] == d['folder'] and o['id'] != g and o['p'] and o['p']['product'] == p['product'] and (o['p']['term'] == p['term'] or o['p']['point'] == p['point']) for o in deals.values())
    for p in M.fresh_reads(cid):
        d = os.path.dirname(p); r = json.load(open(f'{d}/run.json'))
        if not os.path.exists(f'{d}/extract.err'): continue
        try: D = M.load(d, cid)
        except Exception: continue
        S = D['S']
        rec_deals = collections.defaultdict(collections.Counter)
        for rr, cids in S['placed'].items():
            for c in cids:
                for h in S['matches'].get(c, []): rec_deals[rr][D['gold'][h]['deal']] += 1
        main = {g: max(((n[g], rr) for rr, n in rec_deals.items() if n[g]), default=(0, None))[1] for g in S['gold']}
        for u in D['gold']:
            recs = {rr for rr, cids in S['placed'].items() for c in cids if u['i'] in S['matches'].get(c, [])}
            if not recs: continue
            ok = main[u['deal']] in recs and rec_deals[main[u['deal']]].most_common(1)[0][0] == u['deal']
            t = tags.get((u['folder'], u['file'], u['deal']), {})
            ref = 'pointing (implicit/nickname)' if t.get('reference_form') in ('implicit', 'nickname') else 'describing (canonical)'
            sib = 'close siblings' if close(u['deal']) else 'no close sibling'
            tab[(sib, ref)]['right' if ok else 'wrong'] += 1
            tab[(sib, 'renamed' if t.get('rename') else 'not renamed')]['right' if ok else 'wrong'] += 1
for k in sorted(tab):
    c = tab[k]; n = sum(c.values()); print(f"{k[0]:<18} {k[1]:<30} {n:>5}  identity wrong {c['wrong']:>3} ({c['wrong']/n:.1%})")
