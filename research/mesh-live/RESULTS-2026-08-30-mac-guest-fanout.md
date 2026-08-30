# RESULTS — guest fanout (MAC side) — 2026-08-30

Pre-registration: `PRE-REG-guest-fanout-2026-08-30.md`. Bars written before data.

## VERDICT: B1 — SERVED-BY-FANOUT

A guest bearer, dialing FOX's `cwth/guest/0` listener from a fresh random key,
received a real completion for a model FOX does not hold. FOX dispatched to MAC.
**The lender is a gateway to pooled mesh compute, not a lender of its own hardware.**

## Preconditions (enforced by the script, not remembered)

    P1  20:31:40Z  MAC online + RuggedFox online in MAC roster (2/7)   OK
    P2  20:31:40Z  {"ceiling":1,"in_flight":0,"paused_until":null,
                    "yielding_secs_remaining":null}                     OK

P2 was added on the MAC side before firing; the pre-reg lacked it. See "Gap" below.

## Wire result

    FIRE   2026-08-30T20:31:40Z   dial_probe --alpn guest, fresh random key
    RESULT HTTP 200 OK
    BODY   {"id":"chatcmpl-534539","object":"chat.completion",
            "created":1788121902,
            "model":"Qwopus3.5-4B-v3-MTP-Q8_0 @ peer Alexs-MacBook-Pro-2",
            "choices":[{"index":0,"message":{"role":"assistant",
              "content":"The user is asking me to say \"fanout ok\". This seems like a"},
              "finish_reason":"stop"}], ...}

## MAC-side corroboration (independent of FOX's journal)

    20:31:41.725  routing decision path=NamedModel verdict=named_local
    20:31:41.725  mesh-inference: serving complete() locally by explicit model name
    20:31:42.225  inference.complete: done slot="fast" latency_ms=499 tokens_used=41
    20:31:42.225  routing outcome served_by=local:Qwopus3.5-4B-v3-MTP-Q8_0 shed=false

    /status inference.peer_requests
      [{"node_id":"node-44ae76142b0c3c72","name":"RuggedFox",
        "active":0,"served_total":1,"last_request_at":1788121901}]

    /internal/contribution/recent
      InferenceServed for_node=44ae76142b0c3c72 (RuggedFox)
        model_id=Qwopus3.5-4B-v3-MTP-Q8_0 tokens_generated=16 wall_seconds=0.500452792

MAC tallied it through the peer admission path — `max_peer_inflight` was applied,
not bypassed. No shed: log carries no `admission: 503` in the window.

## Two wire-fidelity defects on the fanout path (NOT B3)

Both are the fanout path changing what the client sent/should see. Neither is a
silent model substitution — the correct model served — so this is B1, not B3.

D1  `model` is `"Qwopus3.5-4B-v3-MTP-Q8_0 @ peer Alexs-MacBook-Pro-2"`.
    Not a valid model id. MAC serving the identical request LOCALLY returns the
    clean id `"Qwopus3.5-4B-v3-MTP-Q8_0"` (13:14:xx and 13:26:41 runs). The
    ` @ peer <name>` decoration is added by the fanout path. An OpenAI-compatible
    client that round-trips `model` gets a string no endpoint will accept.
    Strictly, B1 asks for `model` == granted id; it is the granted id + suffix.

D2  `finish_reason` is `"stop"` on a completion truncated at `max_tokens:16`.
    Same request served locally on MAC returns `"length"` — identical content,
    identical truncation point ("...This seems like a"), different verdict.
    Clients use finish_reason to decide whether to continue; reporting "stop"
    for a length-truncated completion silently loses the remainder.

Same class as commit "fix(openai-api): the compatible path stops changing what
the client sent", but on the mesh fanout path rather than the local path.

## Gap in the pre-reg, found before firing

B1-B4 cannot express a BUSY-PEER refusal, and it would have landed as B2.

A local `/v1/chat/completions` on MAC calls `bump_foreground_active()`
(`routes_inference.rs:93`). For `yield_to_foreground_secs=15` that makes
`admit_peer_request` refuse peers with `503 {"reason":"yielded_to_local"}`
(`admission.rs`) AND makes `yield_availability_floor()` publish
`availability: 0.0` to the mesh (`state.rs:1984`), so the router may never
dispatch at all. `yield_peers_to_foreground` defaults true (`state.rs:1742`).
Either path yields a non-200 naming the model unavailable — B2's exact shape,
meaning nothing about the serve path.

Observed live, immediately after this probe:

    posture after: {..., "yielding_secs_remaining":14}
    20:31:47.587  inference_availability TRANSITION previous=1.0 published=0.0
                  activity=1.0 yield_floor=0.0 yielding=true

A concurrent local batch on MAC was held for the probe window for this reason.

Proposed: **B5 — PEER-BUSY REFUSAL = could-not-judge**, gated by P2 above.

## Session note

MAC's roster flapping (FOX read 1/7) was NOT mesh, NOT memory, NOT jetsam:
a second agent session on this host cycled the daemon three times
(20:02:20Z, 20:13:17Z, 20:21:02Z), each a clean `svrn daemon stopped`.
The daemon prints "peak RSS suggests possible jetsam/OOM" on ANY SIGTERM —
that line is not evidence of jetsam, and reading it as such cost a wrong
diagnosis before the graceful-shutdown signature was checked.
