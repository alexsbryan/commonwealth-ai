# Engram hot-set experiment — bars fixed before any data

Question: can the 51.2B-param n-gram embedding table (16 heads x 20M rows x 160
dims) be served from a SMALL resident hot set, with the cold remainder on NVMe
or on a mesh peer?

Geometry taken from the vendor writeup (2026-08-26), treated as spec:
  8 bigram heads + 8 trigram heads, 20,000,000 rows/head, 160 dims/row.

## What is measured
Row-ID access distribution under a uniform hash, over real corpora tokenized
with a real Qwen tokenizer. Hit rate of a STATIC top-K-by-frequency hot set,
mined on one corpus and evaluated on a DISJOINT one.

## Why the hash identity does not need to be the vendor's
Row frequency = n-gram frequency pushed through a many-to-one uniform map.
Any well-distributed hash gives the same CDF SHAPE; only the specific
collision pairs differ. The decision below turns on order of magnitude.

## Pre-registered decision bars (held-out hit rate, all 16 heads)
- GREEN  : <= 2 GiB resident hot set reaches >= 95% ==> tiering is the design.
           Cold remainder may live on NVMe or on a peer; ITL tax = miss x RTT.
- AMBER  : needs 2-16 GiB for 95%  ==> tiering works locally, remote is marginal.
- RED    : needs > 16 GiB for 95%  ==> hot set is NOT a lever. Engram must be
           fully resident; the only remaining lever is quantization.

## Held-out discipline
Mine on sep_mine (even-index SEP articles). Evaluate on:
  - sep_holdout (odd-index SEP articles)  -- in-domain, unseen
  - repo_md                               -- near-domain shift
  - rust_src                              -- hard domain shift
A hot set that only scores on its own mining corpus is reported as FAILED.

## Null / instrument check
Shuffle the token stream (destroys n-gram structure, preserves unigram
frequency). If the shuffled stream shows the same hot-set curve, the curve is
a unigram artifact, not an n-gram result, and the experiment is void.
