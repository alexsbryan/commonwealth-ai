#!/usr/bin/env python3
"""Stream C: minimal-pair grounding data whose label CANNOT be read off the claim.

WHY THIS EXISTS — the defect it is built against, measured 2026-08-06 (note
5698d555). Predict the label from the CLAIM ALONE, no document, bag-of-words +
bigram naive Bayes:

    Stream B (ours)                       AUC 0.8315
    Stream A (HalluGuard-Preferences-76k) AUC 0.8192
    our trained 4B, WITH the document     AUC 0.805

A bag of words that never opens the document beats the verifier that does. The
training task is therefore largely solvable without grounding, which is why
`rewards/accuracies` reaches 1.0 by step 1,000, why AUC parks at ~0.80, and why
step 1,500 scores WORSE than step 1,000 — more steps sharpen a shortcut that
does not transfer.

THE MECHANISM IS TWO GENERATORS, TWO STYLES. Stream B's grounded kinds
(verbatim / reframe / multi_hop_conjunction) and its ungrounded kinds
(unsupported_addition / entity_swap / distractor_absorption …) come from
different constructions, so each side carries a lexical fingerprint. Nothing
ever forced a grounded claim and its ungrounded counterpart to look alike —
`distractor_absorption` sits at 33% document overlap while `verbatim` sits at
94%, and a bag of words separates those trivially without reading anything.

THE FIX IS STRUCTURAL, NOT A FILTER. Every ungrounded claim here is derived
from the grounded claim it is paired with, and BOTH members are emitted. The
twins share ~95% of their tokens, so the marginal distribution of grounded and
ungrounded claim text is identical BY CONSTRUCTION. There is no style to learn;
the only thing distinguishing the pair is whether the document supports the
altered span. `leak_test.py` is the acceptance gate, and this construction is
what passes it.

SHAPE, the secondary gap (same note). Stream B is 100% single-sentence claims,
89% SEP. The card's worst subset, AggreFact-CNN, is 3 sentences / 52 words /
97% overlap and has ZERO representation; claim length correlates -0.54 with
per-subset AUC. So claims here are multi-sentence extractive summaries, and
WHICH sentence carries the corruption is rotated — otherwise a model learns
"check the opener and stop", which is exactly the tpr 100 / tnr 12 behaviour
AggreFact-CNN already elicits.

GROUNDEDNESS IS BY CONSTRUCTION, NOT BY JUDGEMENT. The grounded claim is built
from sentences copied verbatim out of the document, so no model or annotator is
asked to certify it (§7.6: never ask a model to guarantee what code can
enforce). The corruptions are likewise mechanical and the mutated span is
recorded, so every row can be audited later.

Usage:
    build_stream_c.py --out data/stream_c/pilot.jsonl --pairs 400
"""

import argparse
import json
import os
import random
import re
import sys

# Corruptions are chosen so the twin differs by a FEW TOKENS. A corruption that
# rewrites the sentence reintroduces the style tell this file exists to remove.
NUM_RE = re.compile(r"\b\d[\d,]*(?:\.\d+)?\b")
# Substituted entities are drawn from the SAME document, so the corrupted claim
# stays in-vocabulary and in-register — an out-of-document name is detectable as
# an oddity without checking support, which is the shortcut again.
CAP_RE = re.compile(r"\b[A-Z][a-z]{2,}(?:\s+[A-Z][a-z]{2,})*\b")

# EVERY SUBSTITUTION HERE IS BIDIRECTIONAL, AND THAT IS THE WHOLE POINT.
# Measured on the first pilot: `was -> was not` leaked at AUC 1.0000, because
# "not" then appears ONLY in ungrounded claims and a bag of words needs nothing
# else. An antonym swap applied in BOTH directions puts each token on both
# sides of the corpus, so the word itself carries no signal and only its
# relationship to the document does. `entity_swap` already worked this way
# (0.4834, clean) — it draws the replacement from the same document — and that
# is the property being generalised, not a lucky accident.
ANTONYMS = [
    ("increased", "decreased"), ("rose", "fell"), ("before", "after"),
    ("first", "last"), ("more", "less"), ("above", "below"),
    ("began", "ended"), ("gained", "lost"), ("north", "south"),
    ("east", "west"), ("added", "removed"), ("opened", "closed"),
    ("won", "lost"), ("accepted", "rejected"), ("majority", "minority"),
]
QUANTIFIERS = [
    ("some", "all"), ("many", "all"), ("often", "always"),
    ("usually", "always"), ("most", "every"), ("several", "all"),
    ("can", "must"), ("may", "will"),
]


def sentences(text):
    parts = re.split(r"(?<=[.!?])\s+", text.strip())
    return [p.strip() for p in parts if 40 <= len(p) <= 320 and p.count(" ") >= 6]


def perturb_number(s, rng):
    ms = list(NUM_RE.finditer(s))
    if not ms:
        return None
    m = rng.choice(ms)
    raw = m.group()
    try:
        val = float(raw.replace(",", ""))
    except ValueError:
        return None
    # A different value of the SAME FORM: 1970 -> 1974, 12.5 -> 15.5. Changing
    # magnitude or format would be visible without the document.
    if "." in raw:
        new = f"{val + rng.choice([-2.5, -1.5, 1.5, 2.5]):.1f}"
    else:
        delta = rng.choice([-9, -6, -4, 3, 5, 8])
        n = int(val) + delta
        if n <= 0:
            n = int(val) + abs(delta)
        new = f"{n:,}" if "," in raw else str(n)
    return s[:m.start()] + new + s[m.end():], "number_perturb", (m.start(), m.start() + len(new))


def swap_entity(s, doc, rng):
    """Replace an entity with a DIFFERENT entity that appears in the document.

    Intrinsic contradiction: the document names someone, the claim names someone
    else it also mentions. Both names are in-document, so neither the claim's
    vocabulary nor its register betrays which one is right.
    """
    in_claim = {m.group() for m in CAP_RE.finditer(s)}
    in_doc = {m.group() for m in CAP_RE.finditer(doc)}
    cands = sorted(in_claim)
    others = sorted(in_doc - in_claim)
    if not cands or len(others) < 1:
        return None
    tgt = rng.choice(cands)
    rep = rng.choice(others)
    i = s.find(tgt)
    return s[:i] + rep + s[i + len(tgt):], "entity_swap", (i, i + len(rep))


def _bidirectional_swap(s, pairs, kind, rng):
    """Swap a term for its opposite, in whichever direction the sentence allows."""
    opts = []
    for a, b in pairs:
        for x, y in ((a, b), (b, a)):
            m = re.search(rf"\b{x}\b", s, re.I)
            if m:
                opts.append((m, y))
    if not opts:
        return None
    m, rep = rng.choice(opts)
    return s[:m.start()] + rep + s[m.end():], kind, (m.start(), m.start() + len(rep))


def overclaim(s, rng):
    return _bidirectional_swap(s, QUANTIFIERS, "scope_creep", rng)


def flip(s, rng):
    return _bidirectional_swap(s, ANTONYMS, "polarity_flip", rng)


def unsupported_addition(s, doc, rng, foreign):
    """BOTH twins gain a trailing clause; only its SOURCE differs.

    The first pilot appended one of five canned phrases to the ungrounded twin
    only, and leaked at AUC 1.0000 — the phrases WERE the label. The failure is
    general: any clause added to one side alone donates its vocabulary to that
    class. So the grounded twin takes its extra clause from THIS document (still
    supported, since it is the document's own prose) and the ungrounded twin
    takes one from a DIFFERENT article. Both are ordinary encyclopedic English;
    what separates them is support, which is the only thing a verifier should
    be able to use.

    Returns (grounded_sentence, ungrounded_sentence, span) — the only corruption
    that rewrites both members, so it is applied by the caller, not in the loop.
    """
    others = [x for x in sentences(doc) if x != s and 40 <= len(x) <= 200]
    if not others or not foreign:
        return None
    mine = rng.choice(others)
    theirs = rng.choice(foreign)
    if theirs.strip().lower() in doc.lower():
        return None

    def tail(sent, clause):
        base = sent.rstrip()[:-1] if sent.rstrip().endswith(".") else sent.rstrip()
        frag = clause.strip().rstrip(".")
        frag = frag[0].lower() + frag[1:] if frag else frag
        return base + ", and " + frag + "."

    g, u = tail(s, mine), tail(s, theirs)
    return g, u, (len(s), len(u))


def build_pair(doc, sents, rng, foreign=()):
    """One grounded multi-sentence summary and its minimally-corrupted twin."""
    k = rng.choice([2, 3, 3, 4])
    if len(sents) < k:
        return None
    picked = sents[:] if len(sents) == k else rng.sample(sents, k)
    picked.sort(key=lambda x: sents.index(x))       # keep document order
    grounded = " ".join(picked)

    # The two-sided corruption is tried first and rewrites BOTH members, so it
    # cannot run inside the single-sentence loop below.
    if foreign and rng.random() < 0.35:
        idx = rng.randrange(k)
        got = unsupported_addition(picked[idx], doc, rng, foreign)
        if got:
            g_s, u_s, span = got
            off = sum(len(x) + 1 for x in picked[:idx])
            return {
                "grounded": " ".join(picked[:idx] + [g_s] + picked[idx + 1:]),
                "ungrounded": " ".join(picked[:idx] + [u_s] + picked[idx + 1:]),
                "kind": "unsupported_addition",
                "corrupted_sentence": idx,
                "of_sentences": k,
                "span": [off + span[0], off + span[1]],
            }

    # WHICH sentence is corrupted is rotated deliberately. Always corrupting the
    # last one teaches "check the tail"; always the first teaches "check the
    # opener" — the failure AggreFact-CNN already shows at tpr 100 / tnr 12.
    order = list(range(k))
    rng.shuffle(order)
    for idx in order:
        s = picked[idx]
        ops = [lambda: perturb_number(s, rng), lambda: swap_entity(s, doc, rng),
               lambda: overclaim(s, rng), lambda: flip(s, rng)]
        rng.shuffle(ops)
        for op in ops:
            got = op()
            if not got:
                continue
            new_s, kind, span = got
            if new_s == s:
                continue
            # An "ungrounded" sentence that appears verbatim in the document is
            # not ungrounded. Cheap, and it catches perturbations that happen to
            # land on another true value elsewhere in the article.
            if new_s.strip() in doc:
                continue
            twin = picked[:idx] + [new_s] + picked[idx + 1:]
            off = sum(len(x) + 1 for x in picked[:idx])
            return {
                "grounded": grounded,
                "ungrounded": " ".join(twin),
                "kind": kind,
                "corrupted_sentence": idx,
                "of_sentences": k,
                "span": [off + span[0], off + span[1]],
            }
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--index", default=os.path.expanduser(
        "~/.sovereign/indexes/wikipedia/chunks.lance"))
    ap.add_argument("--out", required=True)
    ap.add_argument("--pairs", type=int, default=400)
    ap.add_argument("--scan", type=int, default=40000,
                    help="chunk rows to scan for candidate documents")
    ap.add_argument("--seed", type=int, default=17)
    args = ap.parse_args()

    import lance
    rng = random.Random(args.seed)
    ds = lance.dataset(args.index)

    # Group chunks back into documents. A single 1024-char chunk is too short to
    # summarise in 3 sentences AND still leave unused material for the summary to
    # be a summary OF, so chunks are concatenated per source doc.
    text_f = next(f.name for f in ds.schema
                  if f.name in ("text", "content", "chunk_text", "body"))
    id_f = next(f.name for f in ds.schema
                if f.name in ("source_doc_id", "doc_id", "url", "source"))
    print(f"reading {args.scan} rows: text={text_f} id={id_f}", file=sys.stderr)

    docs = {}
    got = ds.head(args.scan).to_pylist()
    for r in got:
        key = str(r.get(id_f, "")).split("#")[0]
        if not key:
            continue
        docs.setdefault(key, []).append(str(r.get(text_f) or ""))

    n_out = 0
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    keys = sorted(docs)
    rng.shuffle(keys)
    # A pool of real sentences from OTHER articles, for the two-sided
    # unsupported-addition corruption. Real encyclopedic prose, so the added
    # clause cannot be spotted as an oddity without checking the document.
    pool = []
    for kk in keys[:400]:
        pool.extend(sentences("\n\n".join(docs[kk]))[:4])
    with open(args.out, "w") as fh:
        for key in keys:
            if n_out >= args.pairs:
                break
            doc = "\n\n".join(docs[key])
            if len(doc) < 1200:
                continue
            ss = sentences(doc)
            if len(ss) < 6:
                continue
            foreign = [x for x in rng.sample(pool, min(40, len(pool))) if x not in doc]
            pair = build_pair(doc, ss, rng, foreign)
            if not pair:
                continue
            fh.write(json.dumps({
                "doc_id": key, "document": doc[:12000],
                **pair,
            }, ensure_ascii=False) + "\n")
            n_out += 1

    print(f"wrote {n_out} minimal pairs to {args.out} "
          f"(from {len(docs)} documents scanned)", file=sys.stderr)
    return 0 if n_out else 1


if __name__ == "__main__":
    sys.exit(main())
