"""pick_vs_weigh.py <arm>/<corpus> ...: every judged RESOLVE decision, the model's pick beside the weighed zone, and which was right."""
import sys, json, re, collections
tab = collections.Counter(); rows = []
for spec in sys.argv[1:]:
    run = f'runs/loop/{spec}'
    try: D = [json.loads(l) for l in open(f'{run}/decisions.jsonl')]
    except FileNotFoundError: continue
    dec = {}
    for line in open(f'{run}/resolve.err', errors='replace'):
        m = re.search(r'the model\'s choice decides; the weighed zone is kept beside it document="[^"]*" statement=(\S+) decided=(\S+?)(?:\((\d+)[^)]*\))? weighed=(\w+)(?:\((\d+)[^)]*\))?', line)
        if m: dec[m.group(1)] = (m.group(2), m.group(3), m.group(4), m.group(5))
    for d in D:
        v = d.get('verdict')
        if v in (None, 'unjudged'): continue
        st = d['statement']; k = dec.get(st)
        if not k: tab[(spec.split('/')[1], 'no pick-vs-weigh trace', v)] += 1; continue
        decided, di, weighed, wi = k
        agree = (decided == 'NoLink' and weighed == 'NoLink') or (decided.startswith('Link') and weighed.startswith('Link') and di == wi)
        rel = 'agree' if agree else f'pick {decided} vs weigh {weighed}'
        tab[(spec.split('/')[1], rel, v)] += 1
        rows.append((spec, st[-40:], decided, weighed, v))
for k in sorted(tab): print(f"{k[0]:<11} {k[1]:<34} {k[2]:<12} {tab[k]:>3}")
