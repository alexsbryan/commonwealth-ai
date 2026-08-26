#!/usr/bin/env python3
"""Offline article composer over the pooled evidence estate.

AIQ's shape (teardown §1.3 ph.2 per-sub-question dispatch, §6.3 writer
separation), run entirely on the local daemon over evidence this project has
ALREADY fetched. Zero web spend, so the design can be iterated and measured
without burning the DRB-I envelope.

Pipeline per task:
  1. decompose  — the prompt's own sub-questions (from the prompt alone; the
                  frozen criteria are NEVER shown to the writer)
  2. passage    — split estate chunks into ~1.4k-char passages
  3. retrieve   — embed sub-questions + passages, cosine top-k (house method)
  4. write      — one grounded section per sub-question, inline [n] citations
  5. assemble   — report.md + charter.json in the shape score_race.py gates on

Every LLM/embed call is disk-cached by content hash, so re-runs are free and
iteration is tight.
"""
import json, os, re, sys, time, hashlib, argparse, urllib.request, math

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = 'http://127.0.0.1:9741/v1'
CACHE = os.path.join(HERE, '.cache')
QUERY = '/home/alexbryan/dev/deep_research_bench/data/prompt_data/query.jsonl'


def _cache(kind, key, produce):
    os.makedirs(CACHE, exist_ok=True)
    p = os.path.join(CACHE, f'{kind}-{hashlib.sha256(key.encode()).hexdigest()[:24]}.json')
    if os.path.exists(p):
        return json.load(open(p))
    v = produce()
    json.dump(v, open(p, 'w'))
    return v


def post(path, body, timeout=900, tries=40):
    """POST with backoff on the daemon's busy signal.

    The daemon serves ONE request at a time and answers 503
    `local_queue_full` with a `retry_after_secs` when something else
    holds the slot. Only `chat` retried; `embed` did not, so a single
    503 on an embeddings call killed a ten-task run four seconds in.
    Every call retries now, and the wait honours the daemon's own hint.
    """
    data = json.dumps(body).encode()
    for attempt in range(1, tries + 1):
        req = urllib.request.Request(
            BASE + path, data=data, headers={'Content-Type': 'application/json'})
        try:
            return json.load(urllib.request.urlopen(req, timeout=timeout))
        except urllib.error.HTTPError as e:
            wait, why, payload_reason = 20, '', ''
            try:
                raw = e.read().decode()
                payload = json.loads(raw)
                wait = int(payload.get('retry_after_secs') or 20)
                payload_reason = payload.get('reason') or ''
                why = payload_reason or payload.get('error') or raw[:200]
            except Exception:
                why = 'unparseable body'
            transient = (e.code == 429) or (
                e.code == 503 and payload_reason in ('local_queue_full', 'busy'))
            if not transient or attempt == tries:
                # A deterministic backend error is NOT a busy signal.
                # Retrying one forever turns a named defect into a hang.
                raise RuntimeError(f'daemon refused (HTTP {e.code}), not retryable: {why}')
            # A single-slot daemon can hold the slot for a long
            # generation, and a model swap between the embed and chat
            # pins stalls it further. Be patient rather than losing a
            # ten-task run to a transient busy signal.
            wait = min(max(wait, 15) * (1 + attempt // 8), 90)
            print(f'    daemon busy ({e.code}) {why!r}, retry {attempt}/{tries} in {wait}s',
                  file=sys.stderr, flush=True)
            time.sleep(wait)
        except (urllib.error.URLError, TimeoutError) as e:
            if attempt == tries:
                raise
            print(f'    transport error {e}, retry {attempt}/{tries}',
                  file=sys.stderr, flush=True)
            time.sleep(15)
    raise RuntimeError('post: exhausted retries')


def chat(model, prompt, max_tokens=2400, temperature=0.3):
    prompt = scrub(prompt)
    key = f'{model}\x00{max_tokens}\x00{temperature}\x00{prompt}'
    def go():
        for attempt in range(3):
            try:
                o = post('/chat/completions', {
                    'model': model, 'messages': [{'role': 'user', 'content': prompt}],
                    'max_tokens': max_tokens, 'temperature': temperature})
                return {'text': o['choices'][0]['message']['content'],
                        'usage': o.get('usage', {})}
            except Exception as e:
                if attempt == 2:
                    raise
                time.sleep(5)
    return _cache('chat', key, go)


_CTRL = {c: None for c in range(32) if c not in (9, 10, 13)}
_CTRL[0x7f] = None


def scrub(t):
    """Strip C0 control bytes. PDF extraction leaves interior NULs, and the
    embed tokenizer refuses the whole batch on one of them."""
    return t.translate(_CTRL)


def embed(texts, model='commonwealth/embed', batch=32):
    texts = [scrub(t) for t in texts]
    out = []
    for i in range(0, len(texts), batch):
        part = texts[i:i + batch]
        key = model + '\x00' + '\x00'.join(part)
        def go(part=part):
            o = post('/embeddings', {'model': model, 'input': part})
            return [d['embedding'] for d in o['data']]
        out.extend(_cache('emb', key, go))
    return out


def norm(v):
    n = math.sqrt(sum(x * x for x in v)) or 1.0
    return [x / n for x in v]


def cos(a, b):
    return sum(x * y for x, y in zip(a, b))


def strip_think(t):
    return re.sub(r'<think>.*?</think>', '', t, flags=re.S).strip()


# ---------------------------------------------------------------- stages

def decompose(prompt, model):
    """The prompt's own sub-questions. The frozen criteria are never shown."""
    q = ("Read this research request and list the distinct sub-questions a "
         "complete answer must address. Cover every explicit ask and the "
         "implicit background a reader needs. Output ONLY a JSON array of "
         "4-8 short question strings, no prose.\n\nREQUEST:\n" + prompt)
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


def passages(chunks, size=1400, overlap=200):
    out = []
    for c in chunks:
        # ONE decider: scrub where passage text is BORN, not at each
        # consumer. Scrubbing only inside embed() left the writer prompt
        # carrying raw text, and the chat path failed the same way one
        # stage later ("Tokenization failed: interior NUL at byte 7706").
        t = re.sub(r'\s+', ' ', scrub(c['content'])).strip()
        step = size - overlap
        for i in range(0, max(len(t), 1), step):
            seg = t[i:i + size]
            if len(seg) < 220:
                continue
            out.append({'url': c['url'], 'text': seg})
            if len(out) > 4000:
                return out
    return out


def write_section(sub, picked, model, prompt):
    ev = "\n\n".join(f"[{i+1}] ({p['url']})\n{p['text']}" for i, p in enumerate(picked))
    q = (f"You are writing one section of a research report answering:\n{prompt}\n\n"
         f"THIS SECTION answers: {sub}\n\nEVIDENCE:\n{ev}\n\n"
         "Write the section as flowing prose with a short markdown heading. Rules:\n"
         "- Assert ONLY what the evidence supports. Never invent facts, numbers, names or dates.\n"
         "- Cite with bracket numbers matching the evidence above, e.g. [2], after the claim.\n"
         "- Be specific and substantive: name the concrete findings, figures and mechanisms the evidence gives.\n"
         "- If the evidence does not cover part of this sub-question, say so in ONE short closing sentence.\n"
         "- 250-450 words. No preamble, no meta-commentary about the evidence.")
    return strip_think(chat(model, q, max_tokens=1400)['text'])


def compose(task, model, outroot, k=8):
    est = json.load(open(f'{HERE}/estate/task-{task}.json'))
    prompts = {int(json.loads(l)['id']): json.loads(l)['prompt'] for l in open(QUERY)}
    prompt = prompts[task]

    subs = decompose(prompt, model)
    ps = passages(est['chunks'])
    print(f'  task {task}: {len(est["chunks"])} chunks -> {len(ps)} passages, {len(subs)} sub-questions')

    pv = [norm(v) for v in embed([p['text'][:1000] for p in ps])]
    sv = [norm(v) for v in embed(subs)]

    used, sections = {}, []
    for si, sub in enumerate(subs):
        ranked = sorted(range(len(ps)), key=lambda i: -cos(sv[si], pv[i]))
        picked, seen_url = [], {}
        for i in ranked:
            u = ps[i]['url']
            if seen_url.get(u, 0) >= 3:          # source diversity within a section
                continue
            seen_url[u] = seen_url.get(u, 0) + 1
            picked.append(ps[i])
            if len(picked) >= k:
                break
        body = write_section(sub, picked, model, prompt)
        # remap local [n] -> global source numbers
        def remap(m):
            n = int(m.group(1))
            if 1 <= n <= len(picked):
                u = picked[n - 1]['url']
                if u not in used:
                    used[u] = len(used) + 1
                return f'[{used[u]}]'
            return ''
        sections.append(re.sub(r'\[(\d+)\]', remap, body))
        print(f'    §{si+1} {sub[:64]} -> {len(body.split())}w')

    srcs = "\n".join(f'{n}. {u}' for u, n in sorted(used.items(), key=lambda kv: kv[1]))
    report = (f'# {prompt}\n\n' + "\n\n".join(sections) +
              f'\n\n## Sources\n\n{srcs}\n')

    d = f'{outroot}/drb-{task}/dr-lab'
    os.makedirs(d, exist_ok=True)
    open(f'{d}/report.md', 'w').write(report)
    json.dump({'question': prompt}, open(f'{d}/charter.json', 'w'))
    json.dump({'icd': 'verdict-set', 'version': 1, 'run_id': 'dr-lab',
               'charter_hash': 'lab', 'claims': [], 'empty_rounds': []},
              open(f'{d}/verdict-set.json', 'w'))
    print(f'  -> {len(report.split())} words, {len(used)} sources cited')
    return report


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--tasks', default='78')
    ap.add_argument('--model', default='Qwen3.8-27B-UD-Q6_K_XL')
    ap.add_argument('--out', default=os.path.join(HERE, 'out/v1'))
    ap.add_argument('--k', type=int, default=8)
    a = ap.parse_args()
    ids = [int(x) for x in a.tasks.split(',')]
    for t in ids:
        t0 = time.time()
        compose(t, a.model, a.out, a.k)
        print(f'  task {t} done in {time.time()-t0:.0f}s\n')
