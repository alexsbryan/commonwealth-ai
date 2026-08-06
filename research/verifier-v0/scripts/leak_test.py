#!/usr/bin/env python3
"""Can the label be predicted from the CLAIM ALONE, without the document?

THE GATE THIS EXISTS TO BE. A grounding dataset whose labels are readable off
the claim text teaches style detection, not grounding. Measured 2026-08-06
(note 5698d555), word+bigram naive Bayes, 70/30 split:

    Stream B (ours)                       AUC 0.8315
    Stream A (HalluGuard-Preferences-76k) AUC 0.8192
    LLM-AggreFact test card               AUC 0.8157
    our trained 4B, WITH the document     AUC 0.805

The bag of words wins without opening the document. That is why the ORPO
objective saturates (`rewards/accuracies` 1.0 by step 1,000) while real AUC
sits at 0.80 and then DECLINES — the model is sharpening a shortcut.

Read the number as: 0.50 is clean; <=0.55 passes; >0.60 means a model can skip
the document for a meaningful share of the data, and any training run on it is
partly training a style classifier. Run this BEFORE spending GPU time, because
it costs seconds and needs no model.

Deliberately dependency-free and deliberately WEAK — unigrams and bigrams, no
embeddings. A weak learner scoring 0.83 is a much stronger indictment than a
strong learner doing so, and it runs anywhere.

Usage:
    leak_test.py --jsonl data/stream_c/pilot.jsonl --minimal-pairs
    leak_test.py --jsonl <file> --claim-field claim --label-field label
"""

import argparse
import collections
import json
import math
import random
import re
import sys


def toks(s):
    w = re.findall(r"[a-z0-9']+", str(s).lower())
    return set(w) | {f"{a}_{b}" for a, b in zip(w, w[1:])}


def auc_of(scored):
    ss = sorted(scored, key=lambda x: x[0])
    n = len(ss)
    ranks = [0.0] * n
    i = 0
    while i < n:
        j = i
        while j + 1 < n and ss[j + 1][0] == ss[i][0]:
            j += 1
        for k in range(i, j + 1):
            ranks[k] = (i + j) / 2 + 1
        i = j + 1
    pos = sum(1 for _, l in ss if l == 1)
    neg = n - pos
    if not pos or not neg:
        return None
    s = sum(ranks[k] for k in range(n) if ss[k][1] == 1)
    return (s - pos * (pos + 1) / 2) / (pos * neg)


def run(claims, labels, groups=None, seed=17):
    """`groups` keeps both members of a minimal pair on the SAME side of the split.

    WITHOUT IT THE TEST MEASURES MEMORISATION, NOT LEAKAGE. Minimal-pair twins
    share ~95% of their tokens, so a test claim whose twin sat in training
    inherits that twin's label through nearly every token it owns — and since
    the twin carries the OPPOSITE label, the classifier is confidently wrong in
    both directions. Measured on the Stream C pilot: claim-level splitting gave
    AUC 0.0806, i.e. a near-perfect INVERTED classifier, which reads as a
    catastrophic leak and is really an artifact of the split. Pair-level
    splitting is the only way to ask the intended question.
    """
    rng = random.Random(seed)
    if groups is None:
        groups = list(range(len(claims)))
    uniq = sorted(set(groups))
    rng.shuffle(uniq)
    cut = set(uniq[:int(len(uniq) * 0.7)])
    idx = list(range(len(claims)))
    tr = [i for i in idx if groups[i] in cut]
    te = [i for i in idx if groups[i] not in cut]
    cnt = [collections.Counter(), collections.Counter()]
    tot = [0, 0]
    docs = [0, 0]
    for i in tr:
        c = labels[i]
        docs[c] += 1
        for t in toks(claims[i]):
            cnt[c][t] += 1
            tot[c] += 1
    V = len(set(cnt[0]) | set(cnt[1])) or 1

    def score(s):
        """MEAN per-token log-ratio, and no class prior.

        The summed form is LENGTH-BIASED, and the bias is not academic: every
        unseen token contributes log((tot0+V)/(tot1+V)), so whichever class has
        fewer training tokens attracts every long claim regardless of its
        vocabulary. Measured on the Stream C pilot, where the ungrounded twin is
        the longer one: the summed score returned AUC 0.0835 — a perfectly
        INVERTED classifier reading nothing but length. Dividing by the token
        count measures what this test is supposed to measure, which is whether
        the WORDS give the label away.
        """
        ts = toks(s)
        if not ts:
            return 0.0
        acc = 0.0
        for t in ts:
            acc += (math.log((cnt[1][t] + 1) / (tot[1] + V))
                    - math.log((cnt[0][t] + 1) / (tot[0] + V)))
        return acc / len(ts)

    return auc_of([(score(claims[i]), labels[i]) for i in te]), len(te)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jsonl", required=True)
    ap.add_argument("--minimal-pairs", action="store_true",
                    help="rows carry `grounded` and `ungrounded` twins; emit both")
    ap.add_argument("--claim-field", default="claim")
    ap.add_argument("--label-field", default="label")
    ap.add_argument("--pass-at", type=float, default=0.55)
    args = ap.parse_args()

    claims, labels, groups = [], [], []
    with open(args.jsonl) as fh:
        for gi, line in enumerate(fh):
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if args.minimal_pairs:
                if r.get("grounded") and r.get("ungrounded"):
                    claims.append(r["grounded"]); labels.append(0); groups.append(gi)
                    claims.append(r["ungrounded"]); labels.append(1); groups.append(gi)
            else:
                c = r.get(args.claim_field)
                l = r.get(args.label_field)
                if c is None or l is None:
                    continue
                claims.append(c)
                labels.append(int(l) if str(l) in "01" else (0 if l == "grounded" else 1))
                groups.append(gi)

    if len(claims) < 50:
        print(f"too few rows ({len(claims)}) to judge", file=sys.stderr)
        return 2

    auc, n_te = run(claims, labels, groups)
    # JUDGE ON THE DISTANCE FROM CHANCE, IN EITHER DIRECTION. An AUC of 0.08 is
    # exactly as exploitable as 0.92 — invert the classifier and it is 0.92. The
    # first version of this gate printed PASS on 0.0835 from the Stream C pilot,
    # which is a gate that has stopped gating without saying so (§18.1).
    edge = abs(auc - 0.5)
    margin = args.pass_at - 0.5
    print(f"leak test: {args.jsonl}")
    print(f"  claims {len(claims)}  (test split {n_te})")
    print(f"  AUC {auc:.4f}   |AUC-0.5| = {edge:.4f}   [pass when <= {margin:.2f}]")
    print(f"  reference: Stream B 0.8315 · Stream A 0.8192 · our 4B WITH doc 0.805")
    if edge <= margin:
        print("  PASS — the label is not readable from the claim alone.")
        return 0
    direction = "predicts the label" if auc > 0.5 else "predicts it INVERTED (equally exploitable)"
    print(f"  FAIL — a bag of words {direction} without the document. "
          "Training on it teaches style, not grounding.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
