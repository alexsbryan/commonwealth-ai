"""identity_where.py: every identity error in the fresh world reads (no rename on the update), placed at the step that made it."""
import sys, json, os, re, glob, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
where = collections.Counter(); examples = collections.defaultdict(list); seen_runs = 0
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    tags = {(t['folder'], t['file'], t['deal']): t for t in map(json.loads, open(f'{base}/tags.jsonl')) if t['kind'] == 'stage_update'}
    for p in M.fresh_reads(cid):
        d = os.path.dirname(p); rr = json.load(open(f'{d}/run.json'))
        if not os.path.exists(f'{d}/extract.err'): continue
        try: D = M.load(d, cid)
        except Exception: continue
        seen_runs += 1; S = D['S']
        rec_deals = collections.defaultdict(collections.Counter)
        for r, cids in S['placed'].items():
            for c in cids:
                for h in S['matches'].get(c, []): rec_deals[r][D['gold'][h]['deal']] += 1
        main = {g: max(((n[g], r) for r, n in rec_deals.items() if n[g]), default=(0, None))[1] for g in S['gold']}
        owner = {r: n.most_common(1)[0][0] for r, n in rec_deals.items() if n}
        for u in D['gold']:
            if tags.get((u['folder'], u['file'], u['deal']), {}).get('rename'): continue
            cids = [c for r, cs in S['placed'].items() for c in cs if u['i'] in S['matches'].get(c, [])]
            if not cids: continue
            recs = {r for r, cs in S['placed'].items() for c in cs if c in cids}
            g = u['deal']
            if main[g] in recs and owner.get(main[g]) == g: continue
            units = {D['unit_of_cid'].get(c) for c in cids} - {None}
            unit_deals = set().union(*[D['unit_deals'].get(un, set()) for un in units]) if units else set()
            # same window: does the deal also have statements on another particular of this window?
            win = {un[0] for un in units}
            others_same_win = {key for key, ds in D['unit_deals'].items() if g in ds and key[0] in win and key not in units}
            into_other = any(owner.get(r) not in (g, None) for r in recs)
            if into_other and len(unit_deals) > 1: k = 'merge: reader put two deals on one particular'
            elif into_other: k = 'merge: RESOLVE linked a pure particular into another deal'
            elif others_same_win: k = 'split: reader wrote the deal as two particulars in one window'
            else: k = 'split: RESOLVE kept windows apart'
            where[k] += 1
            cl = next((x for x in D['claims'] if (x['ch'], x['about']) in units), None)
            examples[k].append((d.split('/')[2], cid, g, cl['about_name'] if cl else None, u['quote'][:110]))
print(f"{seen_runs} reads; {sum(where.values())} identity errors (rename-free updates)")
for k, n in where.most_common(): print(f"  {n:>3}  {k}")
json.dump(examples, open(os.path.join(os.environ.get('TMPDIR', '/tmp'), 'identity_examples.json'), 'w'), indent=1)
for k, v in examples.items():
    c = collections.Counter((x[1], x[2]) for x in v)
    print(f"\n{k}: deals {dict(c.most_common(10))}")
