import json, os, sys
src = os.path.expanduser("~/.svrnmesh/indexes/wikipedia/atlas/atoms.json")
TOTAL, WANT = 1666146, 220
STEP = TOTAL // WANT
def val(line):
    v = line[line.index(':')+1:].strip()
    return json.loads(v[:-1] if v.endswith(',') else v)
out, cur, idx, in_alias = [], None, -1, False
with open(src, encoding='utf-8') as f:
    for line in f:
        s = line.rstrip('\n')
        if s.startswith('      "atom_type":'):
            if cur is not None and cur['_keep']: out.append(cur)
            idx += 1
            cur = {'_i': idx, '_keep': idx % STEP == 0, 'aliases': [], 'description': ''}
            in_alias = False
            continue
        if cur is None or not cur['_keep']: continue
        if in_alias:
            t = s.strip()
            if t.startswith(']'): in_alias = False
            else: cur['aliases'].append(json.loads(t[:-1] if t.endswith(',') else t))
            continue
        if s.startswith('        "aliases": ['): in_alias = True
        elif s.startswith('        "id":'):            cur['id'] = val(s)
        elif s.startswith('        "canonical_name":'): cur['name'] = val(s)
        elif s.startswith('        "description":'):    cur['description'] = val(s)
        elif s.startswith('          "chunk_id":'):     cur['chunk_id'] = val(s)
        elif s.startswith('          "source_doc_id":'):cur['url'] = val(s)
if cur is not None and cur['_keep']: out.append(cur)
json.dump(out, open("wiki_atom_sample.json","w"))
n = len(out)
empt = sum(1 for a in out if not a['description'])
print(f"scanned {idx+1} atoms · sampled {n}", file=sys.stderr)
print(f"empty description: {empt}/{n} = {100*empt/n:.1f}%", file=sys.stderr)
print(f"with aliases: {sum(1 for a in out if a['aliases'])}/{n}", file=sys.stderr)
ls = sorted(len(a['description']) for a in out)
print("description len percentiles p50/p75/p90/p99/max:",
      ls[n//2], ls[3*n//4], ls[int(.9*n)], ls[int(.99*n)], ls[-1], file=sys.stderr)
