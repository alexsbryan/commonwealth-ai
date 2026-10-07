# Pre-registration: same-vs-related read on structural links (uv-support, tune) — 2026-10-05, before any call

Arm: one case per thread + a maintainer's "Duplicate of #N" + every candidate link (issue cross-reference or a
maintainer's #N, both threads in tune) the local primary reads as `same`, with a verbatim quote code finds in
one of the passages shown. Default `related`. One run, temperature 0.

Reference points (tune): comments baseline B3 F .845 / CEAF-e F .671; perfect read B3 .875 / CEAF-e .730
(30 of the candidate links kept).

MET if B3 F >= .855 AND CEAF-e F >= .690 AND kept-link precision >= .75 (a kept link is right when both threads
share a gold case).
REFUSED if B3 F < .845 OR kept-link precision < .60 (no better than taking every structural link).
Otherwise NOT MOVED. Reported beside: links kept/read, recall of the 30, quotes refused by the code check,
calls and wall seconds.
