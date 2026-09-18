# ring-doc — review findings

One row per finding: principle, path:line, fixed-in hash (or why not fixed).

## REVIEW-audit-rd-1 (2026-09-18, range d8cd7bb9f..18c7f405b)

| # | principle | path:line | fixed in | finding |
|---|---|---|---|---|
| 1 | 5 (gate) | sovereign/crates/sovereign-core/tests/main/f26_egress_census.rs:835 | e88a71212 | `routes_rail_live.rs` (6ac1fd39f) added an HTTP client construction site the F26 census did not know; TESTALL red. Registered Mesh, 1. |
| 2 | 1 | sovereign/crates/sovereign-api/src/routes_rail_live.rs:278 | e88a71212 | `offer_peer` gave up on a peer with no tracing event; the reason reached only the HTTP body. |
| 3 | 1 | sovereign/crates/sovereign-api/src/routes_rail_live.rs:308 | e88a71212 | `live_push`'s 413 and 422 refusals had no tracing event. |
| 4 | 1 | sovereign/crates/sovereign-api/src/routes_internal/ring_live.rs:53 | e88a71212 | the receiver's malformed-envelope refusal was silent while its two sibling refusals warn. |
| 5 | 8 | sovereign/crates/sovereign-grants/src/guest_grant.rs:105, sovereign/crates/sovereign-api/src/server.rs:271, sovereign/crates/sovereign-cli-shared/src/rail.rs:50 | not fixed | `/v1/rail/live` is spelled three times (grant scope, route, client const), extending the same triple spelling append/log already had before the queue. `sovereign-grants` does not depend on `sovereign-cli-shared`, so one const needs a home both layers can read — a design call, carried to NEEDS_HUMAN. Divergence is caught end-to-end by `ring_live_non_durable` (a guest grant drives the route). |
| 6 | 9 | sovereign/crates/sovereign-cli-llm/src/ring_cmd/dev.rs:148 | no change | `upstream` matches four string ops (smell: >3 arms). Kept: it is the one parse of a URL path segment into the route table, and a test asserts the table; an enum would move the same match into `FromStr`. |

Foreign reds seen by this audit (outside the campaign, not fixed here): see
the audit's NEEDS_HUMAN package — conformance tags stale since 30293904f,
`ingest_failure_modes::a_stopped_ingest_is_listed_but_not_usable`,
`every_journey_cites_a_doc_that_exists` (gitignored RING_APPLICATIONS.md
absent on the Halo), arch-gate (AGENTS.md +399 B, approach band +59; campaign
net 0), env-gate (`SOVEREIGN_SIDECAR_FEATURES` declared twice since e3474619c).
