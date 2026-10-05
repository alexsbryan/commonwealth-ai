# Pre-registration: spin-off detection read (uv-support, tune) — 2026-10-05, before any call

Per comment in a tune thread, the local primary answers whether it states something about the thread's own
issue (`this`), raises a different problem or request (`different`, with a verbatim quote code finds in the
comment), or states nothing about any case (`nothing`). Default `this`. One run, temperature 0.

Gold: a comment is a spin-off when its gold case differs from its thread's own case (115 on tune; 105 of them
new cases with no issue of their own). Scored as detection of `different` over comments with a gold case.

MET if precision >= .60 AND recall >= .50 (worth building the grouping pass that turns detections into cases).
REFUSED if precision < .40 (the mail new/given read's level: 23/53) OR recall < .25.
Otherwise NOT MOVED. Reported beside: confusion counts, quotes refused by the code check, calls, wall seconds.
