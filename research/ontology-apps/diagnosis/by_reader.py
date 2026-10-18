"""by_reader.py: deals served by lifecycle class on today's reader (TODAY), the 40k reader of 10-16 (OLD) and the one-shot,
per world; then every deal today's reader misses, with its verdict, the 40k read and the one-shot beside it."""
import json, os, sys, collections, re
sys.path.insert(0, 'research/ontology-apps/loop')
import misses as M
from misses import served

TODAY = {'world-s1': 'leand-0', 'world-s3': 'leand-0', 'world-s2': 'w20-s245', 'world-s4': 'w20-s245', 'world-s5': 'w20-s245'}
OLD = {'world-s1': 'structural-fresh-s1', 'world-s2': 'structural-fresh', 'world-s3': 'structural-fresh',
       'world-s4': 'structural-fresh', 'world-s5': 'structural-fresh'}
LIFE = ['superseded', 'cancel', 'revive']


def flags(c):
    fl = collections.defaultdict(set)
    for t in map(json.loads, open(os.path.expanduser(f'~/.svrnmesh/bench-corpora/{c}/tags.jsonl'))):
        if t['kind'] != 'stage_update': continue
        for f in LIFE + ['rename']:
            if t.get(f): fl[t['deal']].add(f)
    return fl


def cls(fl, g):
    return 'rename' if 'rename' in fl[g] else ('life' if fl[g] else 'plain')


rows = collections.defaultdict(lambda: collections.defaultdict(lambda: [0, 0]))
per_tag = collections.defaultdict(lambda: collections.defaultdict(lambda: [0, 0]))
missed = []
for c in TODAY:
    fl = flags(c)
    reads = {'today': json.load(open(f'runs/loop/{TODAY[c]}/{c}/score.json')),
             '40k': json.load(open(f'runs/loop/{OLD[c]}/{c}/score.json'))}
    O = [json.load(open(f'runs/loop/reference/{c}.{n}-score.json')) for n in ('oneshot', 'oneshot2')]
    for who, S in reads.items():
        for g in S['gold']:
            for key in (c, 'all'):
                rows[(who, key)][cls(fl, g)][0] += 1; rows[(who, key)][cls(fl, g)][1] += served(S, g)
            for f in fl[g] or {'none'}:
                per_tag[who][f][0] += 1; per_tag[who][f][1] += served(S, g)
    for o in O:
        for g in o['gold']:
            for key in (c, 'all'):
                rows[('one-shot x2', key)][cls(fl, g)][0] += 1; rows[('one-shot x2', key)][cls(fl, g)][1] += served(o, g)
            for f in fl[g] or {'none'}:
                per_tag['one-shot x2'][f][0] += 1; per_tag['one-shot x2'][f][1] += served(o, g)
    verdicts = {json.loads(l)['deal']: json.loads(l) for l in open(f'runs/loop/{TODAY[c]}/{c}/misses.jsonl')}
    for g in reads['today']['gold']:
        if served(reads['today'], g): continue
        v = verdicts.get(g, {}).get('verdict', {})
        missed.append((c, g, cls(fl, g), sorted(fl[g]), v.get('mechanism'), v.get('root'), served(reads['40k'], g),
                       sum(served(o, g) for o in O), (verdicts.get(g, {}).get('description') or '')[:70]))

print('deals served: plain | cancel/revive/superseded | renamed')
for key in list(TODAY) + ['all']:
    for who in ('today', '40k', 'one-shot x2'):
        r = rows[(who, key)]
        print(f'  {key:9s} {who:12s} ' + '   '.join(f"{r[k][1]:3d}/{r[k][0]:<3d}" for k in ('plain', 'life', 'rename')))
print('\nby tag (a deal counts under each of its tags)')
for f in ['none', 'superseded', 'cancel', 'revive', 'rename']:
    print(f'  {f:11s} ' + '   '.join(f"{who} {per_tag[who][f][1]}/{per_tag[who][f][0]}" for who in ('today', '40k', 'one-shot x2')))
print('\nmissed by today\'s reader: corpus deal class tags mechanism root | 40k served | one-shot served of 2')
for m in sorted(missed): print('  ', *m)

print('\nnominations (acts under an existing deal; no gold tag): stage claims quoting one, by stage written')
for c in TODAY:
    for who, a in (('today', TODAY[c]), ('40k', OLD[c])):
        D = M.load(f'runs/loop/{a}/{c}', c)
        noms = [cl for cl in D['claims'] if 'nominat' in cl['quote'].lower() and cl['kind'] == 'stage_update' and not cl['refused']]
        docs = sum('nominat' in (b or '').lower() for b in D['body'].values())
        print(f'  {c} {who:6s} messages {docs:2d}  claims {len(noms):2d} {dict(collections.Counter(cl["stage"] for cl in noms))}')
