"""nominations.py <arm>/<world> ...: every generated nomination and chatter message, what the reader wrote, and whether it decided a record."""
import sys, json, os, re, glob, collections
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
tot = collections.Counter()
for spec in sys.argv[1:]:
    arm, cid = spec.split('/')
    run = f'runs/loop/{arm}/{cid}'
    D = M.load(run, cid)
    base = os.path.expanduser(f'~/.svrnmesh/bench-corpora/{cid}')
    doc_of = {k: M.slug(v) for k, v in json.load(open(f'{base}/msgids.json')).items() if v}
    sel = set()
    ap = f'{run}/atlas/atoms.json'
    if os.path.exists(ap):
        atoms = json.load(open(ap)); atoms = atoms if isinstance(atoms, list) else atoms['atoms']
        for a in atoms:
            for rd in ((a['data'].get('attributes') or {}).get('__folds', {}).get('stage', {}).get('readings', [])):
                if rd.get('disposition') == 'selected': sel.add(rd.get('document'))
    gold_files = {(u['folder'], u['file']) for u in D['gold']}
    for p in sorted(glob.glob(f'{base}/story/*/*')):
        folder, file = p.split('/')[-2:]
        txt = open(p, errors='replace').read()
        subj = re.search(r'^Subject:(.*)$', txt, re.M)
        kind = 'nomination' if subj and 'Nominations' in subj.group(1) else None
        if not kind or (folder, file) in gold_files: continue
        doc = doc_of.get(f'{folder}/{file}')
        cls = [c for c in D['claims'] if c['doc'] == doc and c['kind'] == 'stage_update']
        wrote = ','.join(sorted({str(c['stage']) for c in cls})) or 'nothing'
        decides = doc in sel
        tot[(wrote if wrote == 'nothing' else 'stage ' + wrote, decides)] += 1
        print(f"  {cid} {folder}/{file:<4} window={D['where'].get(doc, ('-',))[0]:<10} wrote={wrote:<12} decides_a_record={decides}")
print(dict(tot))
