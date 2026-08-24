#!/usr/bin/env python3
"""Label the replayed admission rows for topicality — the ground truth the
K-sweep and the dedup fix are scored against (campaign drb1-race, acquisition
tune 2026-08-24).

WHAT THIS IS. A pure function over `label-input.jsonl` (the replay harness's
emission — zero web, zero fetch). Each row gets one of three labels for the
charter question it was acquired under:

    on-topic  the source is about the question's subject; a researcher
              answering the question would open it
    adjacent  same field, background, or partial overlap — useful context,
              not an answer
    off       different subject, or not a content page at all

WHY A MODEL AND NOT A RULE. The thing being tuned (`web_hit_relevance`) IS a
rule — distinct query terms present in the hit's surface. Scoring a rule
against ground truth the same rule generated would measure nothing (§18.1: a
check with no failing input you can name). The labeler has to judge meaning,
which is what the operator's thesis puts a small model in a narrow role for.

THE INSTRUMENT'S KNOWN LIMIT, stated up front (§18.4 — validate the instrument
before the result). 773 of the 843 rows carry NO snippet: the logged t7a flight
predates drb1-t1, whose skip ledger records the snippet and query_id. So on 92%
of rows the labeler judges from question + title + url. That is the same
surface the production scorer sees on those rows, so the comparison is fair —
but it is a WEAKER surface than production has at admission time, and any curve
derived from it carries the caveat. The `snippet_source` field rides every
output row so the sweep can partition on it.

THE RATIONALE IS POST-HOC. The model emits the label first, then a reason. The
reason is a legibility aid for the operator's spot-check, not the label's
cause. Read it as "what a reader would say about this decision", not as the
decision's derivation.

DETERMINISM. Greedy (temperature 0, top_p 1), one call per row, no retry that
changes the prompt. Re-running reproduces the file. Resumable: rows already
present in the output are skipped, so an interrupted pass continues.

Usage:
    python3 label_admission.py                       # all 843, fast slot
    python3 label_admission.py --limit 40             # smoke
    python3 label_admission.py --model primary        # 27B cross-check arm
"""
import argparse
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

BASE = 'http://127.0.0.1:9741/v1'
HERE = os.path.dirname(os.path.abspath(__file__))

# The daemon's `max_concurrent_turns` default is 4; past it the REST layer
# sheds 503 (campaign invariant, note 87a3e6cf — AUDIT_CONCURRENCY tracks the
# same number for the same reason).
CONCURRENCY = 4
LABELS = ('on-topic', 'adjacent', 'off')

# Parsimonious by design: a 4B in a narrow role does better with the fewest
# words that fix the task than with a specification it has to hold in working
# memory. The three definitions are the whole rubric.
SYSTEM = (
    "You judge whether a search result is worth reading to answer a research "
    "question.\n"
    "on-topic = about the question's subject; a researcher would open it.\n"
    "adjacent = same field or background; useful context, not an answer.\n"
    "off = different subject, or not a content page.\n"
    "Reply with the label, then a dash, then under 12 words of reason."
)


def prompt_for(row):
    parts = [f"Question: {row['question']}", f"Result title: {row['title']}",
             f"URL: {row['url']}"]
    snippet = (row.get('snippet') or '').strip()
    if snippet:
        parts.append(f"Snippet: {snippet[:600]}")
    parts.append("Label?")
    return "\n".join(parts)


# The daemon sheds 503 when concurrent turns exceed its ceiling. A shed is
# BACKPRESSURE, not a verdict on the row — retrying the identical payload after
# a wait is not a prompt change and does not break determinism. Measured
# 2026-08-24: a 24-row probe at concurrency 8 passed clean, then the 843-row
# pass at the same concurrency shed 828 of 843. One short run is not a
# measurement of a queue ceiling (§18.4) — hence both the backoff AND the drop
# back to the documented concurrency of 4.
SHED_RETRIES = 6


def post(payload, timeout=180):
    req = urllib.request.Request(
        BASE + '/chat/completions',
        data=json.dumps(payload).encode(),
        headers={'Content-Type': 'application/json'})
    delay = 2.0
    for attempt in range(SHED_RETRIES):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            if e.code != 503 or attempt == SHED_RETRIES - 1:
                raise
            time.sleep(delay)
            delay = min(delay * 2, 30.0)
    raise RuntimeError('unreachable')


def parse(text):
    """First matching label word wins; the rest is the reason. A reply that
    names no label is recorded as `unparsed` and counted — NEVER defaulted to
    a class (§18.3: absence is reported, never defaulted)."""
    head = text.strip().lower()
    for lab in LABELS:
        if head.startswith(lab):
            reason = text.strip()[len(lab):].lstrip(' -—:').strip()
            return lab, reason
    for lab in LABELS:
        if lab in head[:40]:
            return lab, text.strip()
    return 'unparsed', text.strip()


def label_row(row, model, stats, lock):
    payload = {
        'model': model,
        'messages': [{'role': 'system', 'content': SYSTEM},
                     {'role': 'user', 'content': prompt_for(row)}],
        'temperature': 0.0,
        'top_p': 1.0,
        'max_tokens': 60,
    }
    t0 = time.time()
    try:
        resp = post(payload)
        text = resp['choices'][0]['message']['content'] or ''
    except (urllib.error.URLError, urllib.error.HTTPError, KeyError,
            TimeoutError, OSError) as e:
        # A transport failure is recorded as a failure, not as a label.
        with lock:
            stats['errors'] += 1
        return dict(row, label='error', reason=f'{type(e).__name__}: {e}',
                    latency_s=round(time.time() - t0, 2))
    label, reason = parse(text)
    with lock:
        stats[label] = stats.get(label, 0) + 1
        stats['done'] += 1
        if stats['done'] % 50 == 0:
            el = time.time() - stats['t0']
            print(f"  {stats['done']}/{stats['n']} rows  {el:.0f}s elapsed",
                  file=sys.stderr, flush=True)
    return dict(row, label=label, reason=reason,
                latency_s=round(time.time() - t0, 2))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--input', default=os.path.join(HERE, 'label-input.jsonl'))
    ap.add_argument('--out', default=os.path.join(HERE, 'admission-labels.jsonl'))
    ap.add_argument('--model', default='fast')
    ap.add_argument('--limit', type=int, default=0)
    ap.add_argument('--concurrency', type=int, default=CONCURRENCY)
    a = ap.parse_args()

    served = json.load(urllib.request.urlopen(BASE + '/models', timeout=60))
    ids = {m['id'] for m in served['data']}
    if a.model not in ids:
        sys.exit(f'exit 2: {a.model!r} is not served locally (have {len(ids)})')

    rows = [json.loads(l) for l in open(a.input) if l.strip()]
    if a.limit:
        rows = rows[:a.limit]

    # Resume: a row is identified by (task, round, rank) — its position in the
    # reconstruction, which is stable across replays.
    done_keys = set()
    if os.path.exists(a.out):
        with open(a.out) as f:
            for line in f:
                if line.strip():
                    r = json.loads(line)
                    if r.get('label') not in ('error', None):
                        done_keys.add((r['task'], r['round'], r['rank']))
    todo = [r for r in rows if (r['task'], r['round'], r['rank']) not in done_keys]
    print(f"labeling {len(todo)} rows ({len(done_keys)} already done) "
          f"on {a.model!r}, concurrency {a.concurrency}", file=sys.stderr)
    if not todo:
        return 0

    stats = {'done': 0, 'errors': 0, 'n': len(todo), 't0': time.time()}
    lock = threading.Lock()
    with open(a.out, 'a') as out, ThreadPoolExecutor(a.concurrency) as pool:
        for res in pool.map(lambda r: label_row(r, a.model, stats, lock), todo):
            out.write(json.dumps(res) + '\n')
            out.flush()

    el = time.time() - stats['t0']
    print(f"\n{len(todo)} rows in {el/60:.1f} min "
          f"({el/max(1,len(todo)):.2f}s/row)", file=sys.stderr)
    for k in list(LABELS) + ['unparsed', 'errors']:
        if stats.get(k):
            print(f"  {k:10s} {stats[k]}", file=sys.stderr)
    return 1 if stats['errors'] else 0


if __name__ == '__main__':
    sys.exit(main())
