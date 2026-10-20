"""identity_pop.py <arm>/<world> ...: every gold update's identity outcome (main record / split off / intruding / not written), by feature and reference cues."""
import sys, json, os, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
rows = []
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    D = M.load(f'runs/loop/{arm}/{cid}', cid); S = D['S']
    tags = {(t['folder'], t['file'], t['deal']): t for t in map(json.loads, open(os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}/tags.jsonl'))) if t['kind'] == 'stage_update'}
    # each record's gold statements by deal, and each deal's main record (most of its matched statements)
    rec_deals = collections.defaultdict(collections.Counter)
    for r, cids in S['placed'].items():
        for c in cids:
            for h in S['matches'].get(c, []): rec_deals[r][D['gold'][h]['deal']] += 1
    main_of = {}
    for g in S['gold']:
        best = max(((n[g], r) for r, n in rec_deals.items() if n[g]), default=None)
        main_of[g] = best[1] if best else None
    owner = {r: n.most_common(1)[0][0] for r, n in rec_deals.items() if n}
    for u in D['gold']:
        recs = {r for r, cids in S['placed'].items() for c in cids if u['i'] in S['matches'].get(c, [])}
        g = u['deal']
        if not recs: out = 'not written'
        elif main_of[g] in recs: out = 'main record'
        elif any(owner.get(r) not in (g, None) for r in recs): out = 'intruding'
        else: out = 'split off'
        t = tags.get((u['folder'], u['file'], g), {})
        rows.append(dict(out=out, cp=t.get('counterparty_form'), ref=t.get('reference_form'), quoted=bool(t.get('quoted_reply')), fwd=bool(t.get('forwarded')),
                         rename=bool(t.get('rename')), sib=(t.get('sibling_deals') or 0) > 0, sup=bool(t.get('superseded')), several=bool(t.get('several_deals_in_message'))))
def show(title, key):
    tab = collections.defaultdict(collections.Counter)
    for r in rows: tab[key(r)][r['out']] += 1
    print(title)
    for k in sorted(tab, key=str):
        c = tab[k]; n = sum(c.values()); wrong = c['split off'] + c['intruding']
        print(f"   {str(k):<34} {n:>4}  main {c['main record']/n:.0%}  split {c['split off']:>2}  intruding {c['intruding']:>2}  not written {c['not written']:>2}  (identity wrong {wrong/n:.0%})")
diverge = lambda r: r['cp'] in ('misspelt', 'alias', 'domain_only', 'implicit') or r['rename']
show('counterparty form x quoted reply', lambda r: (r['cp'], 'quoted' if r['quoted'] else 'not quoted'))
show('features diverge x reference (quoted or forwarded)', lambda r: ('diverge' if diverge(r) else 'aligned', 'ref' if (r['quoted'] or r['fwd']) else 'no ref'))
show('rename / sibling / superseded / several', lambda r: tuple(k for k in ('rename', 'sib', 'sup', 'several') if r[k]) or ('none',))
