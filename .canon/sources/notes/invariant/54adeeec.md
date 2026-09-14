# THE NAMED DISPATCH PATH NEVER CONSULTS sharding, SO A LocalOnly ENVELOPE CROSSES THE TRUST BOUNDARY (M6-B finding B2). Measured 2026-08-06:…

THE NAMED DISPATCH PATH NEVER CONSULTS `sharding`, SO A `LocalOnly` ENVELOPE CROSSES THE TRUST BOUNDARY (M6-B finding B2). Measured 2026-08-06: a non-streaming named request stating `sharding == LocalOnly` was served by peer LittleMac, 200.

Contradicts TWO written contracts:
  - peer_inference.rs:18-19 rule 1 — "No OICP on the request, or `sharding == LocalOnly` -> local."
  - routes_inference.rs:242-268 — the forwarding-boundary gate, "LocalOnly requests must NOT cross the trust boundary."

NEITHER FIRES: the privacy check lives in `offload_verdict`, which named dispatch DELIBERATELY never reaches (peer_inference.rs:1795); and the routes_inference gate sits at Priority 1, AFTER Priority-0 `local_inference` — which IS the mesh-routing provider that forwards. Same §10.6 shape as the bug M1 was written for: a gate written into one call site instead of into the decider.

CENSUS DONE 2026-08-06 — THE FIX IS LOW-RISK. Scanned every non-test, non-example CompletionRequest construction across sovereign/crates, commonwealth/crates and corpus-engine for one carrying BOTH a pinned `model_id` and an OICP envelope: ZERO sites. Internal callers either pin a name with no envelope (CLI/bench/gliner/extract) or attach an envelope with no pinned name (the grounding judges, via `Workload::Judge.requirements(posture)`). The ONLY traffic carrying both is external HTTP — which is exactly the class the gate is meant to protect. So adding a privacy gate to the named path cannot regress an internal caller.

THE RULE TO IMPLEMENT — and note rule 1 as written would BREAK M6-A, so do not implement it literally:
  - NO envelope            -> MAY cross to a peer. This is the thin-client shape and M6-A's passing case; forcing it local would 503 every IDE request for a peer-only model.
  - envelope, LocalOnly    -> must NOT cross. Includes privacy ABSENT, because `sharding()` defaults to LocalOnly and the OICP spec is explicit: "privacy is the default, not something the client has to remember to request. Clients that want distributed inference must opt in."
  - envelope, MeshAllowed  -> MAY cross.
Consequence to accept deliberately: a client that sends an envelope for latency hints but omits privacy loses peer routing. That is the spec's intent, not a regression.

BELT-AND-BRACES INTERACTION: a forwarded named request carries a budget-only envelope whose privacy is absent -> LocalOnly, so this gate would also block a second forward, which the forward budget already blocks. Two independent bounds, which is fine.

MAGNITUDE — LATENT, NOT LIVE. Nothing leaks today (the census is why). But the defense is COINCIDENCE, not structure, which is what §7 forbids. One skill declaring `local_only` while also pinning a model id makes it live.
