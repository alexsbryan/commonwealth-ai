"""hist_wrong.py: partial statements misplaced although their deal's earlier messages were in the window: what the window offered for placing them."""
import sys, json, os, re, glob, collections, io, contextlib
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
sys.path.insert(0, 'research/ontology-apps/diagnosis')
import misses as M
with contextlib.redirect_stdout(io.StringIO()):
    from self_desc import parse, stated
cues = collections.defaultdict(collections.Counter); rows = []
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    doc_of = {k: M.slug(v) for k, v in json.load(open(f'{base}/msgids.json')).items() if v}
    date_of = {}; sender = {}
    for p in glob.glob(f'{base}/story/*/*'):
        t = open(p, errors='replace').read(); k = doc_of.get('/'.join(p.split('/')[-2:]))
        m = re.search(r'^Date:(.*)$', t, re.M)
        if m: date_of[k] = P(re.sub(r"\s*\(.*\)$", "", m.group(1).strip()))
        m = re.search(r'^From:(.*)$', t, re.M); sender[k] = m.group(1).strip().lower() if m else ''
    deals, folder_of = {}, {}
    for f in glob.glob(f'{base}/gold/*.json'):
        g = json.load(open(f))
        for d in g['deals']: deals[d['id']] = (parse(d['description']), [doc_of.get(f"{g['folder']}/{m}") for m in d.get('messages', [])]); folder_of[d['id']] = g['folder']
    for p in glob.glob(f'runs/loop/*/{cid}/score.json'):
        d = os.path.dirname(p); rr = json.load(open(f'{d}/run.json'))
        if rr.get('asker') not in ('daemon', 'recorded') or d.split('/')[2].startswith(('det-', 'carried', 'marked', 'budget', 'header')): continue
        if not os.path.exists(f'{d}/extract.err'): continue
        try: D = M.load(d, cid)
        except Exception: continue
        S = D['S']; W = M.windows(d)
        rec_deals = collections.defaultdict(collections.Counter)
        for r, cids in S['placed'].items():
            for c in cids:
                for h in S['matches'].get(c, []): rec_deals[r][D['gold'][h]['deal']] += 1
        main = {g: max(((n[g], r) for r, n in rec_deals.items() if n[g]), default=(0, None))[1] for g in S['gold']}
        owner = {r: n.most_common(1)[0][0] for r, n in rec_deals.items() if n}
        for u in D['gold']:
            f, msgs = deals.get(u['deal'], (None, []))
            if not f or sum(stated(f[k], u['quote']) for k in ('product', 'vol', 'point', 'term')) == 4: continue
            ch, label = D['where'].get(u['at'], (None, None))
            earlier = [x for x in msgs if x and x != u['at'] and date_of.get(x) and date_of.get(u['at']) and date_of[x] <= date_of[u['at']] and D['where'].get(x, (None,))[0] == ch]
            if not earlier: continue
            cids = [c for r, cs in S['placed'].items() for c in cs if u['i'] in S['matches'].get(c, [])]
            if not cids: continue
            recs = {r for r, cs in S['placed'].items() for c in cs if c in cids}
            ok = main[u['deal']] in recs and owner.get(main[u['deal']]) == u['deal']
            m = re.search(r"^=== " + label + r" ===\n(.*?)(?=^=== d\d+ ===\n|\Z)", W[ch]['prompt'], re.M | re.S)
            body = m.group(1) if m else ''
            own_labels = {D['where'][x][1] for x in earlier}
            pointer = any(f'[quoted from {l}]' in body for l in own_labels)
            subj = (D['subject'].get(u['at']) or '').strip()
            same_subj = any((D['subject'].get(x) or '').strip() == subj and subj for x in earlier)
            same_sender = any(sender.get(x) == sender.get(u['at']) for x in earlier) or any(sender.get(u['at'], '') in (D['body'].get(x) or '') for x in earlier)
            sibs = {o for o in deals if o != u['deal'] and folder_of[o] == folder_of[u['deal']]}
            went = next((owner.get(r) for r in recs if owner.get(r) in sibs), None)
            k = 'right' if ok else 'WRONG'
            for cue, v in (('quote pointer to own earlier message', pointer), ('same subject as an own earlier message', same_subj)):
                cues[(k, cue)][v] += 1
            if not ok:
                cl = next((x for x in D['claims'] if x['doc'] == u['at'] and u['i'] in x['hits']), None)
                rows.append((d.split('/')[2], cid, u['deal'], f'into {went}' if went else 'split off', pointer, same_subj, cl['about_name'] if cl else None, u['quote'][:90]))
for k in sorted(cues): c = cues[k]; t = sum(c.values()); print(f"{k[0]:<6} {k[1]:<40} present {c[True]}/{t} ({c[True]/t:.0%})")
print()
for r in rows: print('  ', r)
