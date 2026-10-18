"""new_thread.py: every gold update, by self-description and by whether its message continues a thread of its deal or opens a new one."""
import sys, json, os, re, glob, collections, io, contextlib
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
sys.path.insert(0, 'research/ontology-apps/diagnosis')
import misses as M
with contextlib.redirect_stdout(io.StringIO()):
    from self_desc import parse, stated
def tkey(s): return re.sub(r'^((re|fw|fwd)\s*:\s*)+', '', (s or '').strip(), flags=re.I).lower()
tab = collections.defaultdict(collections.Counter)
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    doc_of = {k: M.slug(v) for k, v in json.load(open(f'{base}/msgids.json')).items() if v}
    date_of, subj = {}, {}
    for p in glob.glob(f'{base}/story/*/*'):
        t = open(p, errors='replace').read(); k = doc_of.get('/'.join(p.split('/')[-2:]))
        m = re.search(r'^Date:(.*)$', t, re.M)
        if m: date_of[k] = P(re.sub(r"\s*\(.*\)$", "", m.group(1).strip()))
        m = re.search(r'^Subject:(.*)$', t, re.M); subj[k] = tkey(m.group(1)) if m else ''
    deals = {}
    for f in glob.glob(f'{base}/gold/*.json'):
        g = json.load(open(f))
        for d in g['deals']: deals[d['id']] = (parse(d['description']), [doc_of.get(f"{g['folder']}/{m}") for m in d.get('messages', [])])
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
            full = sum(stated(f[k], u['quote']) for k in ('product', 'vol', 'point', 'term')) == 4
            doc = u['at']
            earlier = [x for x in msgs if x and x != doc and date_of.get(x) and date_of.get(doc) and date_of[x] < date_of[doc]]
            if not earlier: th = 'first message of its deal'
            elif subj.get(doc) and any(subj.get(x) == subj.get(doc) for x in earlier): th = 'continues a thread of its deal'
            else: th = 'new thread for an existing deal'
            cids = [c for r, cs in S['placed'].items() for c in cs if u['i'] in S['matches'].get(c, [])]
            if not cids: continue
            recs = {r for r, cs in S['placed'].items() for c in cs if c in cids}
            ok = main[u['deal']] in recs and owner.get(main[u['deal']]) == u['deal']
            tab[('full' if full else 'partial', th)]['right' if ok else 'wrong'] += 1
for k in sorted(tab):
    c = tab[k]; t = sum(c.values()); print(f"{k[0]:<8} {k[1]:<34} {t:>5}  identity wrong {c['wrong']:>3} ({c['wrong']/t:.1%})")
w = sum(c['wrong'] for c in tab.values()); print('all identity errors', w, '; partial + new thread:', tab[('partial', 'new thread for an existing deal')]['wrong'])
