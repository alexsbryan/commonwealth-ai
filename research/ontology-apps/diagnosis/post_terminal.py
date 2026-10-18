"""post_terminal.py: every gold update in the fresh world reads, before or after its deal first reached won/lost, by identity and stage outcome."""
import sys, json, os, glob, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
tab = collections.defaultdict(collections.Counter); deal_tab = collections.defaultdict(lambda: [0, 0])
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
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
            first_t = next((i for i, u in enumerate(ups) if u['stage'] in ('won', 'lost')), None)
            reversal = first_t is not None and first_t < len(ups) - 1
            deal_tab['deal with a reversal (an update after won/lost)' if reversal else 'monotone deal'][0] += not M.served(S, g)
            deal_tab['deal with a reversal (an update after won/lost)' if reversal else 'monotone deal'][1] += 1
            for i, u in enumerate(ups):
                pos = 'after the deal reached won/lost' if first_t is not None and i > first_t else ('the first won/lost' if i == first_t else 'before any won/lost')
                cids = [c for r, cs in S['placed'].items() for c in cs if u['i'] in S['matches'].get(c, [])]
                recs = {r for r, cs in S['placed'].items() for c in cs if c in cids}
                ch = M.chain(D, u)
                if not cids: out = 'not placed'
                elif not (main[g] in recs and owner.get(main[g]) == g): out = 'identity wrong'
                elif ch['status'] == 'stage_misread': out = 'stage misread'
                else: out = 'right'
                tab[pos][out] += 1
for k in ['before any won/lost', 'the first won/lost', 'after the deal reached won/lost']:
    c = tab[k]; n = sum(c.values())
    print(f"{k:<34} {n:>5}  identity wrong {c['identity wrong']/n:.1%}  stage misread {c['stage misread']/n:.1%}  not placed {c['not placed']/n:.1%}")
for k, (m, n) in deal_tab.items(): print(f"{k:<50} deal-reads {n:>4}  missed {m/n:.0%}")
