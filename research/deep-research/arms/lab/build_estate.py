#!/usr/bin/env python3
"""Pool every chunk this project has ever fetched for a DRB-I subset task.

Reads every evidence-window-*.json under research/deep-research/ (927 files
across 37 run roots as of 2026-08-22), keys by the drb-<id> in the path, and
dedupes chunks by (source_url, content-hash). The result is the offline test
bed: real, already-fetched evidence at realistic breadth, so the gate and the
writer can be measured with ZERO web spend.

Output: arms/lab/estate/task-<id>.json
"""
import json, glob, re, os, hashlib, collections

SUBSET = [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]
OUT = os.path.join(os.path.dirname(__file__), 'estate')

def main():
    os.makedirs(OUT, exist_ok=True)
    per = collections.defaultdict(dict)          # task -> key -> chunk
    for f in glob.glob('research/deep-research/**/evidence-window-*.json', recursive=True):
        m = re.search(r'drb-(\d+)', f)
        if not m:
            continue
        t = int(m.group(1))
        if t not in SUBSET:
            continue
        try:
            w = json.load(open(f))
        except Exception:
            continue
        for c in (w.get('chunks') or []):
            content = (c.get('content') or '').strip()
            url = c.get('source_url') or c.get('locator') or ''
            if not content or not url:
                continue
            key = hashlib.sha256((url + '\x00' + content).encode()).hexdigest()[:16]
            if key in per[t]:
                continue
            per[t][key] = {
                'key': key, 'url': url, 'content': content,
                'custody': c.get('custody', 'public-web'),
                'provenance_class': c.get('provenance_class', 'known'),
                'chars': len(content),
            }
    print('%-6s %8s %9s %12s' % ('TASK', 'SOURCES', 'CHUNKS', 'CHARS'))
    for t in SUBSET:
        chunks = sorted(per[t].values(), key=lambda c: -c['chars'])
        urls = {c['url'] for c in chunks}
        json.dump({'task': t, 'sources': sorted(urls), 'chunks': chunks},
                  open(f'{OUT}/task-{t}.json', 'w'), indent=1)
        print('%-6d %8d %9d %12d' % (t, len(urls), len(chunks), sum(c['chars'] for c in chunks)))

main()
