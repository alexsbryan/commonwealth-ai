"""tag_tab.py <arm>/<world> ...: miss rate per trap tag, of each deal's deciding update and of any of its updates."""
import sys, json, os, collections
sys.path.insert(0, 'research/ontology-apps/loop')
from misses import served
DYN = ["rename", "turnover", "handoff", "renegotiation", "term_change", "correction", "superseded", "regress", "stall", "cancel", "revive", "quoted_reply", "forwarded", "several_deals_in_message", "internal"]
last_t = collections.defaultdict(lambda: [0, 0]); any_t = collections.defaultdict(lambda: [0, 0]); tot = [0, 0]
missed = []
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    S = json.load(open(f'runs/loop/{arm}/{cid}/score.json'))
    tags = [json.loads(l) for l in open(os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}/tags.jsonl'))]
    from email.utils import parsedate_to_datetime as P
    import re
    gold_ups = {}
    for f in os.listdir(os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}/gold')):
        g = json.load(open(os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}/gold/{f}')))
        for u in g['stage_updates']: gold_ups.setdefault(u['deal'], []).append(dict(u, folder=g['folder']))
    tag_of = {(t['folder'], t['file'], t['deal']): t for t in tags if t['kind'] == 'stage_update'}
    for g in S['gold']:
        ok = served(S, g); tot[0] += ok; tot[1] += 1
        ups = sorted(gold_ups[g], key=lambda u: P(re.sub(r"\s*\(.*\)$", "", u['date'])))
        ts = [tag_of.get((u['folder'], u['file'], g)) for u in ups]
        last = ts[-1] or {}
        keys = [f"ref={last.get('reference_form')}", f"cp={last.get('counterparty_form')}", f"siblings={'0' if not last.get('sibling_deals') else '1+'}"]
        keys += [d for d in DYN if last.get(d)]
        for k in keys: last_t[k][0] += ok; last_t[k][1] += 1
        anyk = {d for t in ts if t for d in DYN if t.get(d)}
        for k in anyk: any_t[k][0] += ok; any_t[k][1] += 1
        if not ok: missed.append((spec, g, keys, sorted(anyk)))
print(f"all deals: served {tot[0]}/{tot[1]}, miss {(tot[1]-tot[0])/tot[1]:.0%}")
print("deciding update's tags:")
for k, (s, n) in sorted(last_t.items(), key=lambda kv: -(kv[1][1]-kv[1][0])/max(1, kv[1][1])):
    print(f"   {k:<26} missed {n-s:>2}/{n:<3} {(n-s)/n:.0%}")
print("any update of the deal carries:")
for k, (s, n) in sorted(any_t.items(), key=lambda kv: -(kv[1][1]-kv[1][0])/max(1, kv[1][1])):
    print(f"   {k:<26} missed {n-s:>2}/{n:<3} {(n-s)/n:.0%}")
for m in missed: print('  MISS', m)
