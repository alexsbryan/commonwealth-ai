#!/usr/bin/env python3
"""T5 verifier prototype — embed-locate, then judge.

The production binder decides support with `chunk.content.contains(s)`. That
is brittle string matching: 91% of logged claims bound to nothing because
research prose paraphrases its sources. This is the replacement, in the shape
the operator directed (the embed-router centroid method) and composed so that
no stage is asked a question it cannot answer:

  stage 1  figures      a claim's digits must appear verbatim in the cited
                        source. A number is a feature of the claim's FORM,
                        not its vocabulary. Deterministic, honesty-critical.
  stage 2  locate       embed the claim and the cited source's spans; the
                        candidate span is the argmax cosine. This replaces
                        contains(). It answers only "which part of this
                        source is about this claim".
  stage 3  decide       the located span goes to the judge, which answers
                        "does this span SUPPORT or CONTRADICT the claim".
                        Cosine cannot see negation: "affects more men than
                        women" and "more women than men" are neighbours in
                        embedding space, so similarity alone would bind a
                        contradicting source and manufacture grounding.

A source counts as an origin only when all three agree. A claim's own
[n] marker never counts on its own — it only selects which source is examined.
"""
import json, os, re, sys, argparse, collections

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from compose import chat, embed, norm, cos, strip_think  # noqa: E402

MIN_SIM = 0.35          # locate floor; below this nothing is "about" the claim
SPAN = 900


def spans_of(text, size=SPAN, overlap=250):
    t = re.sub(r'\s+', ' ', text).strip()
    step = size - overlap
    return [t[i:i + size] for i in range(0, max(len(t), 1), step) if len(t[i:i + size]) > 150]


def figures(s):
    s = re.sub(r'\[\d+\]', ' ', s)                       # citation handles never contribute
    return set(re.findall(r'\d+(?:\.\d+)?%?', s))


def supports(span, claim, model):
    q = (f"Passage:\n{span}\n\nStatement: {claim}\n\n"
         "Does the passage support the statement? Answer exactly one word: "
         "yes, no, or contradicts.")
    a = strip_think(chat(model, q, max_tokens=6, temperature=0.0)['text']).strip().lower()
    if a.startswith('y'):
        return 'support'
    if a.startswith('c'):
        return 'contradict'
    return 'unsupported'


def verify_report(report_path, estate_path, model, out_json):
    rep = open(report_path).read()
    est = json.load(open(estate_path))
    by_url = collections.defaultdict(str)
    for c in est['chunks']:
        by_url[c['url']] += ' ' + c['content']

    m = re.search(r'## Sources\s*\n\n(.*)$', rep, re.S)
    srcs = {}
    if m:
        for line in m.group(1).strip().splitlines():
            mm = re.match(r'(\d+)\.\s+(\S+)', line.strip())
            if mm:
                srcs[int(mm.group(1))] = mm.group(2)

    body = rep[:m.start()] if m else rep
    sents = [s.strip() for s in re.split(r'(?<=[.!?])\s+', body) if s.strip()]
    cited = [s for s in sents if re.search(r'\[\d+\]', s)]
    print(f'  {len(sents)} sentences, {len(cited)} carry citations, {len(srcs)} sources')

    # one embed pass over every span we might need
    span_cache = {}
    for n, u in srcs.items():
        span_cache[n] = spans_of(by_url.get(u, ''))
    flat, index = [], []
    for n, sp in span_cache.items():
        for j, s in enumerate(sp):
            flat.append(s[:1000]); index.append((n, j))
    vecs = [norm(v) for v in embed(flat)] if flat else []
    pos = {k: i for i, k in enumerate(index)}

    cvecs = [norm(v) for v in embed([re.sub(r'\[\d+\]', '', s)[:1000] for s in cited])] if cited else []

    rows = []
    for ci, s in enumerate(cited):
        claim = re.sub(r'\[\d+\]', '', s).strip()
        ns = sorted({int(x) for x in re.findall(r'\[(\d+)\]', s)})
        fig = figures(s)
        origins, detail = set(), []
        for n in ns:
            u = srcs.get(n)
            if not u:
                continue
            src_text = by_url.get(u, '')
            # stage 1 — figures verbatim
            fig_ok = all(f in src_text for f in fig) if fig else True
            # stage 2 — locate
            best, bsim = None, -1.0
            for j, sp in enumerate(span_cache.get(n, [])):
                k = pos.get((n, j))
                if k is None:
                    continue
                sim = cos(cvecs[ci], vecs[k])
                if sim > bsim:
                    bsim, best = sim, sp
            if best is None or bsim < MIN_SIM:
                detail.append({'src': n, 'sim': round(bsim, 3), 'verdict': 'no-span'})
                continue
            # stage 3 — decide
            v = supports(best, claim, model) if fig_ok else 'figure-absent'
            detail.append({'src': n, 'sim': round(bsim, 3), 'verdict': v})
            if v == 'support':
                origins.add(u)
        rows.append({'claim': claim[:400], 'cited': ns, 'figures': sorted(fig),
                     'origins': len(origins), 'detail': detail})
        if (ci + 1) % 10 == 0:
            print(f'    verified {ci+1}/{len(cited)}', flush=True)

    n = len(rows) or 1
    o1 = sum(1 for r in rows if r['origins'] >= 1)
    o2 = sum(1 for r in rows if r['origins'] >= 2)
    contra = sum(1 for r in rows if any(d['verdict'] == 'contradict' for d in r['detail']))
    summary = {'claims': len(rows), 'verified_1plus': o1, 'verified_2plus': o2,
               'contradicted': contra,
               'pct_verified': round(100 * o1 / n, 1), 'pct_2plus': round(100 * o2 / n, 1)}
    json.dump({'summary': summary, 'rows': rows}, open(out_json, 'w'), indent=1)
    print(f'  VERIFIED >=1 origin: {o1}/{len(rows)} ({summary["pct_verified"]}%) | '
          f'>=2: {o2} ({summary["pct_2plus"]}%) | contradicted: {contra}')
    return summary


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--task', type=int, required=True)
    ap.add_argument('--report', required=True)
    ap.add_argument('--model', default='Qwen3.8-27B-UD-Q6_K_XL')
    ap.add_argument('--out', default=None)
    a = ap.parse_args()
    out = a.out or a.report.replace('.md', '.verify.json')
    verify_report(a.report, f'{HERE}/estate/task-{a.task}.json', a.model, out)
