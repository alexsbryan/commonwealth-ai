# Resolver precision — the answer is no, and it is not close

**Certified-claims-skip-judge does NOT ship.** The span resolver certifies
a claim with precision **0.7429** against the incumbent's verdicts. The
bar, pinned before any of this data was scored, was **0.98**. Roughly one
in four certifications would be a released wrong "Grounded" badge.

Per the work order, that makes this deliverable a success: the
measurement is the deliverable, per-claim verification stays exactly as
it is, and D3 wires no judge-skip.

## The numbers

Offline replay, 2026-08-09. No model loaded, no judge called, no turn
run — `resolve_span` is a pure function of `(span, chunks)`. Re-running
produced byte-identical artifacts (verified).

Frozen inputs (sha256 in `resolver_precision_verdict.json`; the
transcripts live on `skunkworks/native-grounding` and are not duplicated
onto main):

| transcript | what it is |
|---|---|
| `saltgrass_longneg_20260808.transcripts.jsonl` | tonight's longform negatives harvest |
| `saltgrass_compound_longneg_20260808.transcripts.jsonl` | its compound-question arm |
| `secret_agent_gv_shadow_20260807.transcripts.jsonl` | the secret_agent gv-shadow run |

130 claims, every one of them judged by the incumbent (no `fail_open`,
no `unverified`, so nothing was excluded as could-not-judge). 103
verified, 27 `failed_once` — and `failed_once` means **not supported**,
which is the single most misreadable field in this replay.

|  | incumbent verified | incumbent failed |
|---|---|---|
| **resolver certified** | 26 | **9** |
| resolver did not | 77 | 18 |

- **precision** `P(verified | certified)` = **0.7429** — bar 0.98. Missed
  by a distance no threshold tweak closes.
- **coverage** `P(certified | verified)` = 0.2524 (0.2680 excluding the 6
  verified claims whose turn carried no evidence pool at all).

## Why it fails, which matters more than that it fails

**`Verbatim` resolution fires on 4 claims out of 130, and the incumbent
failed all four.**

| incumbent | words | claim |
|---|---|---|
| failed | 6 | `Sluice gate ironwork wants seeing to` |
| failed | 3 | `the inn's back` |
| failed | 8 | `the only satisfaction that week had offered him` |
| failed | 3 | `the lock-keeper's apprentice,` |

These are not claims. They are claim-extraction fragments — short noun
phrases that appear verbatim in a chunk *because they are short*, not
because a proposition was grounded. The addressable-span rule scores
**precision 0.000** on this data: restricting to `Verbatim`, the only
thing it certifies is extraction noise.

Every genuine full-sentence claim that the resolver certifies at all
comes back **`Fuzzy`** — present in the pool by the shipped presence
kernel, but scattered across it, with no contiguous span in any single
chunk to point at. All 26 true certifications are `Fuzzy`, and so are 5
of the 9 false ones.

That is the finding underneath the number: **`Fuzzy` is not evidence a
proposition is supported.** It means the claim's *words* occur in the
evidence. A confabulated sentence assembled out of vocabulary the
passages genuinely contain resolves `Fuzzy` just as readily as a true
one — which is exactly what the 9 false certifications are, and exactly
what a longform-negatives bank is built to produce.

## What this does and does not license

**Does not:** skipping the judge for certified claims, at any operating
point on this data. Both rules were scored and both are reported. There
is no third rule hiding between them — `Verbatim` and `Fuzzy` are the
only two certification outcomes the resolver has.

**Does not, either:** reading this as a defect in `span_resolver`. The
module's own documentation is careful that `Fuzzy` is "a
resolution-quality distinction, not a second opinion about grounding",
and it is right. This measurement asked whether that distinction is
*load-bearing enough to replace a judge*, and the answer is no. The
resolver is doing what it says; the proposed use was the overreach.

**Does:** segments as DISPLAY, which is what D4 builds. A `Fuzzy`
segment truthfully says "these words are in your sources, but I cannot
point at where" and a `Verbatim` one hands over an address. Both are
honest renderings; neither is a verdict. Nothing in this measurement
argues against showing a reader provenance — it argues against letting
provenance stand in for verification.

**Consistent with H4's own gate**, which returned `could_not_judge` on
its held-out set for a related reason (23 supported / 0 unsupported —
single-class). This run had a real negative class (27 of 130), which is
why it could return a verdict where H4 could not, and the verdict is
negative.

## Reproducing

```
svrn bench resolver-precision --repo-root <path-to-skunkworks-worktree>
```

Exit 3 = below the bar. Exit 0 would mean it cleared; exit 4 means the
label set could not support a verdict.
