#!/usr/bin/env python3
"""Composer — AIQ's writer contract (teardown §1.6, §6.3) over our estate.

v4 (2026-08-23) un-throttles the writer. v3 handed each section k=8
passages (~2.8k tokens) out of a 10-52k-token estate and asked for
300-380 words, producing 2.3-3.3k-word articles against references of
6.9-13.3k. Both ceilings were ours: the short article was chosen to fit
a 32,764-token JUDGE window that has since been raised to 65,536. Task
78 measured the cost directly — 4,526 words scored 44.59, the same
pipeline at 2,752 words scored 42.85. Declared in
adversarial/pre-registration.md §"V4 the un-throttled writer".

v1 measured coverage. v2 targets INSIGHT, which carries the highest mean
dimension weight across the DRB-I subset (0.351) and is our worst dimension
(1.71 vs the reference's 9.19). The criteria that carry that weight ask for
evaluation, synthesis, justification and significance — not for facts listed.

Ported from AIQ §6.3, each item a prompt obligation:
  - synthesis map before drafting (components -> facts that must survive,
    consensus vs conflict, table-vs-prose)
  - retain detail; do NOT flatten rich notes into generic themes
  - cross-synthesize across sources into higher-level conclusions
  - present conflicts, saying which evidence is stronger or more recent
  - developed paragraphs, not checklists
  - err toward more useful information rather than less
  - name real gaps; citations map 1:1 to fetched sources

Still zero web spend: every passage comes from the pooled estate.
"""
import json, os, re, sys, time, argparse, hashlib

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from compose import (chat, embed, norm, cos, strip_think, passages, QUERY)  # noqa: E402

CONTRACT = """Obligations for this section:
- Retain the useful detail: specific numbers, dates, names, mechanisms, study
  findings and caveats from the evidence must survive into the prose. Do NOT
  flatten them into generic themes.
- Cross-synthesize ACROSS sources into higher-level conclusions rather than
  summarising one source at a time.
- Do not merely report: evaluate. Say what the finding means, why it matters,
  how strong the support is, and what follows from it.
- Where sources disagree, present the conflict and say which evidence is
  stronger or more recent.
- Developed paragraphs, not bullet checklists. A short markdown table is
  welcome where the content is genuinely tabular.
- Err on the side of more useful information rather than less.
- Where the domain has a canonical taxonomy, scale, standard or level-system
  that organizes this material AND the evidence names it, use it explicitly
  and say what each level or category means. Do not paraphrase around a named
  framework the sources actually use.
- Where sources differ in rigor, currency or independence, say which is
  stronger and why. Where a claim has been challenged, corrected, contaminated
  or retracted in the evidence, say so.
- Assert ONLY what the evidence supports. Never invent facts, numbers, names
  or dates. Cite with bracket numbers, e.g. [2], after each material claim.
- If the evidence genuinely does not cover part of this sub-question, do not
  drop it and do not lead with it: write everything the evidence DOES support
  first, then close the section with one short sentence naming what the
  sources do not reach."""



def tidy_citations(text):
    """Collapse [5][5] -> [5] and sort runs: [6][5] -> [5][6]."""
    def fix(m):
        ns = sorted({int(x) for x in re.findall(r'\d+', m.group(0))})
        return ''.join(f'[{n}]' for n in ns)
    return re.sub(r'(?:\[\d+\]){2,}', fix, text)


def decompose2(prompt, model):
    q = ("Read this research request and list the sections a comprehensive, "
         "analytical report must contain to answer it fully. EVERY explicit "
         "ask in the request — including each numbered or enumerated "
         "requirement — must map to at least one section, and no explicit ask "
         "may be merged away. Add the background and the evaluative synthesis "
         "a demanding reader expects. Output ONLY a JSON array of 5-8 short "
         "section titles phrased as questions, no prose.\n\nREQUEST:\n" + prompt)
    raw = strip_think(chat(model, q, max_tokens=700, temperature=0.2)['text'])
    m = re.search(r'\[.*\]', raw, re.S)
    if m:
        try:
            subs = [s.strip() for s in json.loads(m.group(0)) if isinstance(s, str) and s.strip()]
            if subs:
                return subs[:8]
        except Exception:
            pass
    return [l.strip(' -*0123456789.') for l in raw.splitlines() if len(l.strip()) > 12][:8]


def write_section2(sub, picked, model, prompt, words=(700, 850), max_tokens=2400):
    ev = "\n\n".join(f"[{i+1}] ({p['url']})\n{p['text']}" for i, p in enumerate(picked))
    q = (f"You are writing ONE section of an analytical research report that answers:\n{prompt}\n\n"
         f"THIS SECTION: {sub}\n\nEVIDENCE:\n{ev}\n\n{CONTRACT}\n\n"
         f"Write {words[0]}-{words[1]} words. Start with a '## ' heading; use '### ' "
         "sub-headings where the material has natural parts. No preamble, no "
         "meta-commentary about the evidence or about being an AI.")
    return strip_think(chat(model, q, max_tokens=max_tokens)['text'])


def write_synthesis(prompt, sections, model):
    digest = "\n\n".join(s[:3000] for s in sections)
    q = (f"You are writing the closing synthesis of a research report answering:\n{prompt}\n\n"
         f"THE REPORT SO FAR:\n{digest}\n\n"
         "Write a '## Synthesis and Assessment' section of 500-600 words that:\n"
         "- draws the threads together into 3-5 justified conclusions, each stating "
         "  WHY it follows from what the report established;\n"
         "- weighs which conclusions rest on strong evidence and which are tentative;\n"
         "- names the genuine open questions and what would resolve them;\n"
         "- offers the practical implication a demanding reader would want.\n"
         "Reuse the bracket citation numbers already used above where a claim needs one. "
         "Developed paragraphs, no checklists, no new facts beyond what the report states.")
    return strip_think(chat(model, q, max_tokens=1600)['text'])


def compose2(task, model, outroot, k=28, repeat_cap=5, words=(700, 850)):
    est = json.load(open(f'{HERE}/estate/task-{task}.json'))
    prompts = {int(json.loads(l)['id']): json.loads(l)['prompt'] for l in open(QUERY)}
    prompt = prompts[task]

    subs = decompose2(prompt, model)
    ps = passages(est['chunks'])
    print(f'  task {task}: {len(est["chunks"])} chunks -> {len(ps)} passages, {len(subs)} sections', flush=True)
    pv = [norm(v) for v in embed([p['text'][:1000] for p in ps])]
    sv = [norm(v) for v in embed(subs)]

    used, sections = {}, []
    for si, sub in enumerate(subs):
        ranked = sorted(range(len(ps)), key=lambda i: -cos(sv[si], pv[i]))
        picked, seen = [], {}
        for i in ranked:
            u = ps[i]['url']
            if seen.get(u, 0) >= repeat_cap:
                continue
            seen[u] = seen.get(u, 0) + 1
            picked.append(ps[i])
            if len(picked) >= k:
                break
        body = write_section2(sub, picked, model, prompt, words=words)

        def remap(m):
            n = int(m.group(1))
            if 1 <= n <= len(picked):
                u = picked[n - 1]['url']
                used.setdefault(u, len(used) + 1)
                return f'[{used[u]}]'
            return ''
        sections.append(tidy_citations(re.sub(r'\[(\d+)\]', remap, body)))
        print(f'    §{si+1} {sub[:60]} -> {len(body.split())}w', flush=True)

    synth = write_synthesis(prompt, sections, model)
    synth = re.sub(r'\[(\d+)\]', lambda m: m.group(0) if int(m.group(1)) <= len(used) else '', synth)
    sections.append(tidy_citations(synth))
    print(f'    §synthesis -> {len(synth.split())}w', flush=True)

    srcs = "\n".join(f'{n}. {u}' for u, n in sorted(used.items(), key=lambda kv: kv[1]))
    report = f'# {prompt}\n\n' + "\n\n".join(sections) + f'\n\n## Sources\n\n{srcs}\n'

    d = f'{outroot}/drb-{task}/dr-lab'
    os.makedirs(d, exist_ok=True)
    open(f'{d}/report.md', 'w').write(report)
    json.dump({'question': prompt}, open(f'{d}/charter.json', 'w'))
    # The knobs travel WITH the arm. v2 is unreproducible from this file
    # because its word budget was edited in place before v3 ran; an arm
    # whose settings live only in the working tree is not a measurement.
    json.dump({'k': k, 'repeat_cap': repeat_cap, 'words': list(words),
               'sections': len(subs), 'model': model,
               'passages_available': len(ps),
               'evidence_chars_per_section': k * 1400,
               'contract_sha': hashlib.sha256(CONTRACT.encode()).hexdigest()[:16]},
              open(f'{d}/arm.json', 'w'), indent=1)
    json.dump({'icd': 'verdict-set', 'version': 1, 'run_id': 'dr-lab',
               'charter_hash': 'lab', 'claims': [], 'empty_rounds': []},
              open(f'{d}/verdict-set.json', 'w'))
    print(f'  -> {len(report.split())} words, {len(used)} sources\n', flush=True)
    return report


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--tasks', default='78')
    ap.add_argument('--model', default='Qwen3.8-27B-UD-Q6_K_XL')
    ap.add_argument('--out', default=os.path.join(HERE, 'out/v2'))
    ap.add_argument('--k', type=int, default=28)
    ap.add_argument('--repeat-cap', type=int, default=5)
    ap.add_argument('--words', default='700,850')
    a = ap.parse_args()
    for t in [int(x) for x in a.tasks.split(',')]:
        t0 = time.time()
        compose2(t, a.model, a.out, a.k, a.repeat_cap,
                 tuple(int(x) for x in a.words.split(',')))
        print(f'  task {t} in {time.time()-t0:.0f}s', flush=True)
