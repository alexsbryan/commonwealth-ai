import sys, json, os, re, glob, collections, io, contextlib
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
sys.path.insert(0, 'research/ontology-apps/diagnosis')
import misses as M
with contextlib.redirect_stdout(io.StringIO()):
    from self_desc import parse, stated
tab = collections.defaultdict(collections.Counter); ex = collections.defaultdict(list)
for cid in ['world-s1', 'world-s2', 'world-s3', 'world-s4', 'world-s5']:
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    doc_of = {k: M.slug(v) for k, v in json.load(open(f'{base}/msgids.json')).items() if v}
    date_of = {}
    for p in glob.glob(f'{base}/story/*/*'):
        m = re.search(r'^Date:(.*)$', open(p, errors='replace').read(), re.M)
        if m: date_of[doc_of.get('/'.join(p.split('/')[-2:]))] = P(re.sub(r"\s*\(.*\)$", "", m.group(1).strip()))
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
        for u in D['gold']:
            f, msgs = deals.get(u['deal'], (None, []))
            if not f or sum(stated(f[k], u['quote']) for k in ('product', 'vol', 'point', 'term')) == 4: continue
            if any(u['i'] in S['matches'].get(c, []) for cs in S['placed'].values() for c in cs): continue
            ch = D['where'].get(u['at'], (None,))[0]
            hist = any(x and x != u['at'] and date_of.get(x) and date_of.get(u['at']) and date_of[x] < date_of[u['at']] and D['where'].get(x, (None,))[0] == ch for x in msgs)
            c = M.chain(D, u); k = 'history' if hist else 'no history'
            tab[k][c['status']] += 1
            if len(ex[(k, c['status'])]) < 3: ex[(k, c['status'])].append((d.split('/')[2], cid, u['folder'] + '/' + u['file'], u['quote'][:80], [cl['quote'][:60] for cl in c['spans'] + c['claims']][:1]))
for k, c in tab.items(): print(k, dict(c.most_common()))
for k, v in sorted(ex.items()):
    if k[1] in ('short_span', 'not_emitted', 'other_sentence_cited'):
        print(k); [print('    ', x) for x in v]
