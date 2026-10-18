import sys, json, os, re, glob, collections, io, contextlib
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
sys.path.insert(0, 'research/ontology-apps/diagnosis')
import misses as M
with contextlib.redirect_stdout(io.StringIO()):
    from self_desc import parse, stated
tab = collections.defaultdict(collections.Counter); where = collections.defaultdict(collections.Counter)
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    doc_of = {k: M.slug(v) for k, v in json.load(open(f'{base}/msgids.json')).items() if v}
    date_of = {}
    for p in glob.glob(f'{base}/story/*/*'):
        m = re.search(r'^Date:(.*)$', open(p, errors='replace').read(), re.M)
        if m: date_of[doc_of.get('/'.join(p.split('/')[-2:]))] = P(re.sub(r"\s*\(.*\)$", "", m.group(1).strip()))
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
        S = D['S']
        rec_deals = collections.defaultdict(collections.Counter)
        for r, cids in S['placed'].items():
            for c in cids:
                for h in S['matches'].get(c, []): rec_deals[r][D['gold'][h]['deal']] += 1
        main = {g: max(((n[g], r) for r, n in rec_deals.items() if n[g]), default=(0, None))[1] for g in S['gold']}
        owner = {r: n.most_common(1)[0][0] for r, n in rec_deals.items() if n}
        for u in D['gold']:
            f, msgs = deals.get(u['deal'], (None, []))
            if not f: continue
            partial = sum(stated(f[k], u['quote']) for k in ('product', 'vol', 'point', 'term')) < 4
            ch = D['where'].get(u['at'], (None,))[0]
            before = lambda x: x and x != u['at'] and date_of.get(x) and date_of.get(u['at']) and date_of[x] <= date_of[u['at']]
            hist = any(before(x) and D['where'].get(x, (None,))[0] == ch for x in msgs)
            sibs = [o for o in deals if o != u['deal'] and folder_of[o] == folder_of[u['deal']]]
            sib_in = any(before(x) and D['where'].get(x, (None,))[0] == ch for o in sibs for x in deals[o][1])
            cids = [c for r, cs in S['placed'].items() for c in cs if u['i'] in S['matches'].get(c, [])]
            if not cids: continue
            recs = {r for r, cs in S['placed'].items() for c in cs if c in cids}
            ok = main[u['deal']] in recs and owner.get(main[u['deal']]) == u['deal']
            into_sib = any(owner.get(r) in sibs for r in recs)
            k = ('partial' if partial else 'full', 'own history' if hist else 'no own history', 'sibling in view' if sib_in else 'no sibling in view')
            tab[k]['right' if ok else ('into a sibling' if into_sib else 'other wrong')] += 1
for k in sorted(tab):
    c = tab[k]; t = sum(c.values())
    print(f"{k[0]:<8} {k[1]:<15} {k[2]:<19} {t:>5}  wrong {(t - c['right'])/t:.1%}  (into a sibling {c['into a sibling']}, other {c['other wrong']})")
