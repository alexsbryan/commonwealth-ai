# Portfolio composed after-arm — 21 desktop turns, build 688f8eba (2026-08-14)

Same instrument/protocol as the baseline (4cb8ee5c): launchd one-shot, bridge
:9745, fresh conversation per turn, quiet box (seat claim held), turn 0
warmup excluded (150.4s). Desktop relaunched at 688f8eba (= c3ae4428
tombstone + 688f8eba drafter input-health). Forensics:
gate_audit_forensics_20260814_portfolio_afterarm.jsonl (181 rows).

## Walls (n=20 warm) — against baseline same-day, same instrument

|            | BASELINE (pre-land) | AFTER (landed set) | delta |
|---|---|---|---|
| median     | 95.75s | **72.2s** | **-23.5s (-25%)** |
| p90        | 118.2s | **91.3s** | -26.9s |
| min / max  | 52.8 / 157.4 | 52.7 / 113.6 | tail -43.8s |
| wall cv    | —      | 0.212 | |

**BAR (median <=75 AND p90 <=90): median PASSES for the first time in the
initiative's history; p90 misses by 1.3s. Formal verdict on E-wall-time:
FAILED on the p90 clause — judged, not passed — with the median clause met.**

## Composition — the mechanism, visible

actions: released 7 / annotated_marked 13 / rewrite_* 0 / re_audit 0.
THE TOMBSTONE HELD 20/20: zero rewrite or re-audit executions (the
E-tombstone-ledger clause-(a) after-check: rewrite/re_audit rows 0 of N,
against 16 of 21 on the baseline).
released walls: 52.9-87.1 (median ~66).
annotated_marked walls: 52.7-113.6, median 76.9 — the band that was
84-157s when these turns paid rewrite + re-audit.
Clean rate 7/20 (35%) vs 5/20: Fisher p=0.73 — not distinguishable at this
n; the drafter changes' sensitive instrument is per-class failure counts at
audit#1 (D3 scoring over the forensics), not the binary clean rate.

## The remaining tail

p90 is decided by 3 marked turns (91.3 / 96.3 / 113.6; 113.6 is the first
post-warmup turn). The tail is now audit#1 + draft bound — no repair
machinery remains in it.
