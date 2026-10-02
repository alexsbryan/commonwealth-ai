<!-- ledger -->

**phase-c-5 · 2026-10-01 · pc-onprem-followups · worker** — this commit
- Needed: a keyed daemon's OCR cleanup admitted without a loopback exemption, and a sealed turn's offers derived from the registry.
- Chose: a per-process self credential in the one key store (not an in-process call), and a corpus-only `search` that reports `Scope::Persistent`.
- Because: the cleanup already speaks HTTP to `/v1/chat/completions`, so one store row reuses the one admission decision; and `Scope::External` on a tool that cannot leave the machine was itself the false claim the decider reads.

<!-- appendix -->

## phase-c-5 · 2026-10-01 — the daemon admits itself by key; a corpus-only search says so in its scope

<details><summary>reasoning, evidence, package</summary>

Item 1 (OCR on a keyed daemon). The row allowed "an in-process call or the
daemon's own credential, never a loopback exemption" (phase-b-86). An
in-process call would need a second path into the chat route's handler
(the turn admission, the slot resolution by file stem) that the HTTP path
already owns, so it would be a second implementation of route dispatch.
The credential is one row in `ClientTokenStore`: `self_credential()` is
minted once per process from `generate_bearer_token` (the one bearer
generator), never written to disk, and is admitted only by a store the
disk made keyed. Its sub `@svrn` is not a label, so no key file can claim
it. Falsified if a keyed daemon admits a caller presenting no key, or an
unkeyed daemon admits `@svrn` (test
`the_self_credential_admits_only_on_a_keyed_store`).

Item 3 (sealed web offer). The decider `web_search_in_reach` reads the
built registry's descriptors, so the only honest input is the descriptor.
`SearchTool::new` (no web fallback) reported `Scope::External`, whose
definition is "effect reaches outside this machine"; it now reports
`Persistent` and a corpus-only description. No reader gates on `External`
(census: only label renderers in cli-dev, core executor and planner), so
the change moves no behaviour beyond the turn's prompt. Falsified if an
open host's `search` stops reporting `External` (test
`a_corpus_only_search_describes_no_web`).

</details>
