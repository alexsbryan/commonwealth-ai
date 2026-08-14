# D2-smoke gap analysis — the scan number was never real, the search grew under our feet (order audit-economy, directive 6fdc5796)

2026-08-14, post-restart, box idle. Instruments: the smoke census
(`~/.svrnmesh/journal/grounding-2026-08-14.jsonl`, 18:13-18:18Z rows, per-call
mechanism/ms/prompt_chars/out_chars/start_offset_ms), the daemon log
(prefix_state + per-completion rows, both pre- and post-restart windows —
the 09:16Z rotation covers both), `runs/audit-economy-d2-smoke/forensics.jsonl`
(52 claim rows), and one fresh 18-call replay of the A' scan register on the
idle box (18:41-18:44Z). All model calls local.

## Headline

The smoke's 25.39s stage median against the <=16.8s decider decomposes into
one confirmed win, one number that was never real, and one term that grew for
reasons outside this order. The <=16.8s bar is NOT reachable from the order's
remaining in-scope levers alone; the arithmetic is at the bottom.

## 1. The scan gap: hypothesis falsified — the 1.17s was a misextraction

The steer's hypothesis (replay prices the call with the family pin warm;
live pays family prefill) is **falsified on this arm**:

- Every scored live scan rode a `prefix_state: HIT` of the judges' family
  pin (key `3b4389a9d12c54fd`, restored 5,996 tok, restore 26-67 ms; zero
  LEARN/MISS for the family across the arm). Live does not pay the family
  prefill on this bank. (Fresh-question traffic still would, once per turn
  — that caveat from D0 stands, but it is not what happened here.)
- The real error is in the D3-A analysis: **"candidate scan 1.17s median"
  was a row-misattribution** — 1.17s is exactly the *batched* register's
  median (out ~30 chars), and the pre-restart daemon log for the D3-A
  window (15:40-16:15Z) shows the actual candidate scan calls at
  **2.8-10.5s, median ~8.4s** (tokens ~8.0-8.5K, out 372-1,773 chars). A
  400-token decode cannot fit in 1.17s at any plausible rate; the claim
  was arithmetically impossible as written and nobody caught it.
- Reproduction, post-restart, idle box: the same 9 cases x2 through the
  same harness = **median 8.35s**. And the item lists are **9/9
  byte-identical to the D3-A run across the daemon restart** (same 7/10
  labeled score, same Kane miss) — the register's verdicts are bit-stable;
  only its cost claim falls.

**The live scan cost model** (fits the smoke, the fresh replay, and the
pre-restart window): ~3.1s floor (pin restore + ~1.9-2.2K-token suffix
prefill under turn load) + ~4 ms per emitted char of flagged items
(decode ~55-65 tok/s). Clean turns pay ~3.1s (out=NONE); marked turns pay
5-8.3s for their findings. A' therefore delivered exactly what its
mechanism could deliver: the own-family full prefill became a ~40ms
restore, 9.7s -> 5.6s live (-4.1s). The -8.5s projection was built on a
number that never existed.

**Instrument lesson, sized honestly (this is narrower than "replay
underprices everything"):** replay VERDICTS are solid (bit-stable across
a daemon restart, 9/9). Replay COST numbers are decode-conditioned and
extraction-fragile. D1's batched 1.17s was genuine (out ~30 chars by
construction) and the smoke CONFIRMED its projection live (batch calls
1.1-1.4s; call-sum 4.46s vs 6.5s amended / 5.5s original bar — both met).
Procedural fix: any replay cost claim must cite (tokens_used,
response_chars) beside latency, so a 1.17s claim over a 400-token decode
cannot be written.

## 2. The +3.3s non-call growth: two co-factors, neither is D2 misbehaving

- **The steer's candidate is confirmed as a FACT but it is not the
  growth:** all 29 batch-cleared claims still ran their per-claim corpus
  search (forensics: searched=true, extras non-empty, 29/29) and nothing
  consumes the result on a cleared claim. This waste is not new — the
  pre-D2 path searched-then-judged every claim; D2 removed the judge
  call, not the search. It is the ladder's exact target, now worth ~69%
  of the audited-claim searches on batched turns (29 of 42).
- **The growth itself tracks the same-day retrieval slowdown** (backlog
  59c2a82a): per-audited-claim search cost rose from ~0.75-0.83s (D0) to
  ~0.9-1.4s in the arm, while the retrieval STAGE in the same turns rose
  4.8s -> 8.15s median — same corpus-search substrate, same ~+60-70%
  inflation. Root cause is outside this order (chunk counts / lancedb
  index decay are the backlog item's candidates).
- Minor third term: scan-item corrective searches in the post-scan tail
  (~0.6-3s on marked turns, scales with flagged items).

Fair-comparison note for the decider: at D0-era search prices the same
smoke arm reads ≈ 22.1s. The order's levers delivered -10.7s of mechanism
wins (judges -6.6s, scan -4.1s); the environment gave +3.3s back.

## 3. Re-pricing against live-measured terms (stage median 25.39s)

| lever | status | delta (live prices) | stage after |
|---|---|---|---|
| ladder search-skip | staged, gated on lost_rescue=0 | -4 to -5s | ~20.5-21.5s |
| + retrieval-slowdown reversal (59c2a82a) | OUTSIDE this order | -2.5 to -3.3s | ~18-19s |
| + scan decode trim (max_items/brevity) | NEW judge-input candidate, unpriced, replay-first + quality bars | -1 to -2.5s (marked turns) | ~16.5-18s |
| + claim_list decode trim / fast-slot | NEW candidate, unpriced, riskiest | -1.5 to -3s | ~15-17s |

**Plain statement: <=16.8s is not reachable from the order's remaining
in-scope levers.** The ladder alone lands ~20.5-21.5s; even in a repaired
retrieval environment, ladder-only reads ~18-19s. The bar comes into
range only if the ladder AND the external retrieval fix AND at least one
new quality-gated register change all land at full value. Per the steer's
point 5: the not-worth-continuing clause does not cover this case (the
batched register did not fail — it met both its bars), so the
continue/close-short decision is the operator's, with these numbers.

**Recommended next measurement if the order continues:** the ladder
shadow (`runs/audit-economy-ladder-shadow/`, baseline verdicts, separate
seat-owned desktop launch) — largest in-scope lever, and its lost_rescue
safety evidence is required for any future flip regardless of this
order's close.

## Corrections landed with this analysis

- `audit_economy_d3a_scan_family_20260814.md` — cost section corrected
  (misattributed 1.17s; quality verdict untouched).
- `audit_economy_d3b_window_narrow_20260814.md` — "A's 1.17s floor"
  comparisons corrected; B remains refused (0/3 should_flag stands on its
  own; its 8.6s cost is now read against A's true ~5.6s live, still
  dominated).
