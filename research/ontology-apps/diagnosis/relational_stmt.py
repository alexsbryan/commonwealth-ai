"""relational_stmt.py: every gold stage update in the generated worlds' current reads, by its tag dynamics, and what the reader did with it."""
import sys, json, os, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
REL = ["superseded", "cancel", "revive", "renegotiation", "several_deals_in_message", "rename", "term_change", "correction", "regress"]
tab = collections.defaultdict(collections.Counter)
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    D = M.load(f'runs/loop/{arm}/{cid}', cid)
    tags = {(t['folder'], t['file'], t['deal']): t for t in map(json.loads, open(os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}/tags.jsonl'))) if t['kind'] == 'stage_update'}
    for u in D['gold']:
        t = tags.get((u['folder'], u['file'], u['deal']), {})
        dyn = [d for d in REL if t.get(d)]
        k = 'relational: ' + '+'.join(dyn) if dyn else 'plain'
        c = M.chain(D, u)
        ok = c['status'] == 'emitted' and any(u['deal'] in (M.served.__module__ and [D['S']['gold'][u['deal']]['records']] and [u['deal']]) for _ in [0])
        right = c['status'] == 'emitted' and bool(set(c['records']) & set(D['S']['gold'][u['deal']]['records']))
        tab['plain' if not dyn else 'relational']['right' if right else c['status']] += 1
        for d in dyn: tab['  ' + d]['right' if right else c['status']] += 1
for k, c in tab.items():
    n = sum(c.values()); print(f"{k:<28} {n:>4}  read right {c['right']/n:.0%}   wrong: {dict((s, v) for s, v in c.most_common() if s != 'right')}")
