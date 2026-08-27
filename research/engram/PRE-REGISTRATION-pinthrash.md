# Pre-registration — the pin-thrash fix

Registered 2026-08-27, before the post-fix run. Bars fixed here.

## The bug (measured, 2026-08-26 20:55 synth run)

`plan_directed` fell through to "learn at MY declared boundary" whenever the
stored entry was not a strict prefix of this request. Two request shapes sharing
a 48-token family fingerprint but diverging before either declaration therefore
evicted each other forever, each eviction costing a FULL prefill:

  key                 LEARN HIT  pin sequence
  e5421d00596bab13        4   5  [3998, 4612, 3998, 4612]
  d31fc622ff749915        4   4  [6176, 6936, 6176, 6936]
  46bccb0019378802        4   2  [5493, 4813, 5493, 4813]
  d61dc5db068a5ce2        3   7  [5335, 5978, 5335]
  d8cdc9d72820c7ea        3   3  [5115, 4515, 5115]
  ccba09283f273f3f        1   4  [3893]              <- the only stable key
  TOTAL: 19 LEARN, 25 HIT. 12 of 19 LEARNs were thrash ~= 240s of that run's
  566s of cold prefill (42%).

## The fix

When the entry cannot serve this request but the two share >= min_pin tokens,
pin at their COMMON PREFIX and mark the key in `shared_pins`; the upgrade rule
then declines to lengthen it. A genuinely drifted family shares only the
fingerprint, falls under min_pin, and still replaces the pin as before.

## Registered bars — post-fix re-run of the SAME lane

`bench all --synth --filter sep --sample-questions 5`, single tenant, guarded.

- WIN      NO key's pin sequence contains a repeated value (the direct
           structural signature of thrash is GONE), AND total LEARN <= 10
           (vs 19), AND HIT >= 30 (vs 25).
- PARTIAL  thrash gone on some keys but >= 1 key still repeats a pin size.
           Report which and why; do not claim the fix.
- NO-GO    any key still oscillates A-B-A-B, or total LEARN >= 19.
- VOID     quality moves: facts strict must stay 34/35 +/- 1. Restore is
           bit-faithful, so a quality change means the fix broke correctness
           and the throughput result is irrelevant. This bar OUTRANKS the others.

Wall-clock is NOT a bar. It is confounded by daemon state, page cache and
whatever else the box is doing; the LEARN/HIT counts are the direct measurement
of the mechanism. Wall time is reported as context only.
