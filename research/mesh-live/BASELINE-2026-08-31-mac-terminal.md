# MAC baseline — terminal two-machine run, pre-registered 2026-08-31T22:30:05Z

Captured BEFORE any FOX data exists, so the F6 corroboration is a delta against a
recorded value rather than a story told afterwards.

| Surface | Value at handoff |
|---|---|
| `/status.peer_requests` | `[]` |
| `/internal/contribution/status` (port **9742**, not 9741) | `{"ceiling":1,"in_flight":0,"paused_until":null,"yield_peers_to_foreground":true}` |
| node id (full) | `37f17554b6c4ff292af4844ad4dbc43c` |
| node_class | `holder` |
| advertises | `Qwopus3.5-4B-v3-MTP-Q8_0` (loaded) |
| embed slot | `qwen-embedding-0.6b` |
| mesh | Meshsonics, 2/7 online (MAC, RuggedFox) |
| join key | `cwth-bf7e-d2dd-8efc`, exp 1788301576 |

## The bar MAC itself must clear after FOX runs F6

`peer_requests` is `[]` now. After the terminal serves one turn it MUST contain an
entry naming the terminal's node id, and the contribution ledger MUST carry an
`InferenceServed` for it.

This is the half that makes the result two-machine rather than FOX's self-report:
FOX saying "I got a completion" and MAC saying "I served it for node X" are
independent observations, and only the pair rules out a local fallback that
happened to look right.

## Known confound, guarded in the brief

`yield_peers_to_foreground: true` with `ceiling: 1`. Local activity on MAC stamps a
~15s window in which peer dispatch is refused `503 yielded_to_local`. The brief
classifies that as could-not-judge, never as a failed binding. Do not run local
chat/completions on MAC while FOX is firing F6-F8.
