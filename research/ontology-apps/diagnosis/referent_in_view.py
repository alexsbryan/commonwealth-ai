"""referent_in_view.py <arm>/<world> ...: each relational statement (tagged gold update, or a nomination), read right or not,
by whether the deal's previous message is in the statement's window."""
import sys, json, os, re, glob, collections
from email.utils import parsedate_to_datetime as P
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
REL = ["superseded", "cancel", "revive", "renegotiation", "rename", "term_change", "correction", "regress"]
tab = collections.defaultdict(collections.Counter)
def pd(s): return P(re.sub(r"\s*\(.*\)$", "", s.strip()))
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    D = M.load(f'runs/loop/{arm}/{cid}', cid); S = D['S']
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    doc_of = {k: M.slug(v) for k, v in json.load(open(f'{base}/msgids.json')).items() if v}
    date_of = {}
    for p in glob.glob(f'{base}/story/*/*'):
        m = re.search(r'^Date:(.*)$', open(p, errors='replace').read(), re.M)
        if m: date_of[doc_of.get('/'.join(p.split('/')[-2:]))] = pd(m.group(1))
    tags = {(t['folder'], t['file'], t['deal']): t for t in map(json.loads, open(f'{base}/tags.jsonl')) if t['kind'] == 'stage_update'}
    def prev_in_window(deal, doc):
        msgs = [doc_of.get(f"{D['deals'][deal]['folder']}/{m}") for m in D['deals'][deal].get('messages', [])]
        before = [m for m in msgs if m and m != doc and date_of.get(m) and date_of.get(doc) and date_of[m] < date_of[doc]]
        if not before: return 'no previous message'
        prev = max(before, key=lambda m: date_of[m])
        return 'previous in window' if D['where'].get(prev, (None,))[0] == D['where'].get(doc, (None,))[0] else 'previous outside window'
    for u in D['gold']:
        t = tags.get((u['folder'], u['file'], u['deal']), {})
        if not any(t.get(d) for d in REL): continue
        c = M.chain(D, u)
        right = c['status'] == 'emitted' and bool(set(c['records']) & set(S['gold'][u['deal']]['records']))
        tab[('relational update', prev_in_window(u['deal'], u['at']))]['right' if right else 'wrong'] += 1
    gold_files = {(u['folder'], u['file']) for u in D['gold']}
    for p in glob.glob(f'{base}/story/*/*'):
        folder, file = p.split('/')[-2:]
        if (folder, file) in gold_files: continue
        txt = open(p, errors='replace').read(); subj = re.search(r'^Subject:(.*)$', txt, re.M)
        if not (subj and 'Nominations' in subj.group(1)): continue
        doc = doc_of.get(f'{folder}/{file}')
        deal = next((g for g, d in D['deals'].items() if d['folder'] == folder and file in d.get('messages', [])), None)
        if not deal: deal = next((g for g, d in D["deals"].items() if d["folder"] == folder), None)
        if not deal: continue  # a hay folder: no gold deal to read it against
        cls = [c for c in D['claims'] if c['doc'] == doc and c['kind'] == 'stage_update']
        right = all(c['stage'] == 'won' for c in cls)
        tab[('nomination', prev_in_window(deal, doc))]['right' if right else 'wrong'] += 1
for k in sorted(tab):
    c = tab[k]; n = sum(c.values()); print(f"{k[0]:<18} {k[1]:<24} {n:>3}  wrong {c['wrong']:>2} ({c['wrong']/n:.0%})")
