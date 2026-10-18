"""span_tab.py <arm>/<corpus> ...: every gold deal, served or not, by how many windows its gold updates span."""
import sys, json, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
rows = []
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    run = f'runs/loop/{arm}/{cid}'
    D = M.load(run, cid)
    S = D['S']
    for g in sorted(S['gold']):
        ups = sorted([u for u in D['gold'] if u['deal'] == g], key=lambda u: u['dt'])
        wins = [D['where'].get(u['doc'], (None,))[0] for u in ups]
        span = len({w for w in wins if w})
        last_w = wins[-1]
        first_w = wins[0]
        ok = M.served(S, g)
        rows.append(dict(run=spec, deal=g, served=bool(ok), span=span, updates=len(ups),
                         last_in_first=(last_w == first_w), hidden=sum(w is None for w in wins)))
tab = collections.defaultdict(lambda: [0, 0])
for r in rows:
    k = ('1 window' if r['span'] <= 1 else '2+ windows')
    tab[k][0] += r['served']; tab[k][1] += 1
for k in sorted(tab): print(f"{k:<12} served {tab[k][0]}/{tab[k][1]}  missed {tab[k][1]-tab[k][0]}")
tab2 = collections.defaultdict(lambda: [0, 0])
for r in rows: tab2[r['span']][0] += r['served']; tab2[r['span']][1] += 1
print('by span:', {k: f"{v[0]}/{v[1]}" for k, v in sorted(tab2.items())})
json.dump(rows, open(os.path.join(os.environ.get('TMPDIR', '/tmp'), 'span_tab.json'), 'w'), indent=1)
