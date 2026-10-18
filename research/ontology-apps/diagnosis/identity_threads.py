import sys, json, os, re, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
def tkey(subj): return re.sub(r'^((re|fw|fwd)\s*:\s*)+', '', (subj or '').strip(), flags=re.I).lower()
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    S = json.load(open(f'runs/loop/{spec}/score.json'))
    c = M.corpora.corpus(cid); story = c.get('story') or os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}/story')
    file_of = {M.slug(v): k for k, v in json.load(open(c['msgids'])).items() if v}
    def thread(doc):
        f = file_of.get(doc)
        if not f or not os.path.exists(f'{story}/{f}'): return None
        s = re.search(r'^Subject:(.*)$', open(f'{story}/{f}', errors='replace').read(), re.M)
        return f.split('/')[0] + ':' + tkey(s.group(1) if s else '')
    deals = {d['id']: d for d in S['deals']}
    for l in open(f'runs/loop/{spec}/misses.jsonl'):
        r = json.loads(l); m = r['verdict']['mechanism']
        if m not in ('in_window_split', 'cross_window_split', 'in_window_merge', 'cross_window_merge', 'misattributed', 'shared_sentence_misrouted'): continue
        recs = [x['id'] for x in r['records']]
        print(f"{cid} {r['deal']:<18} {m}")
        for rid in recs:
            ths = collections.Counter(thread(S['claim_doc'].get(cid_)) for cid_ in S['placed'].get(rid, []))
            print(f"     record {rid[-8:]} gold={deals[rid]['gold']} strays={deals[rid]['strays']}: threads {dict(ths)}")
