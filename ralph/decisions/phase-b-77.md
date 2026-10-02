<!-- ledger -->

**phase-b-77 · 2026-09-30 · pb-mesh-exit-transport-claims (renamed) · seat** — this commit
- Needed: phase-b-76 ruled the switchover's three forks as the seat would have. It minted the capabilities row as `pb-rails-renew-claims`, but that name split no scoped row, so the frozen-scope rule (phase-b-32) held it as "waits on the operator", and it would never have dispatched. Meanwhile the switchover, marked `[~]`, resumed ahead of it, because a `[~]` row resumes before any other. Its atomic commit could then land without svrn's capability claims, which would silently drop this node's corpora and embed model from what peers see (principle 6).
- Chose:
  - The seat stopped the switchover session two minutes in, with no commit and a clean tree.
  - The row is renamed `pb-mesh-exit-transport-claims`: it is a split of the switchover, carved from its census by phase-b-76.
  - The switchover is reset to `[ ]`, so the claims row runs first.
  - The supervisor is relaunched with the manifest's launch line.
  - phase-b-76's substance stands unchanged: the renew door takes claims, renewals run every 10 s, the guest ALPN is kept, and `partition` lives in kernel-types.
- Because:
  - Scope splits carry the parent's prefix.
  - A prerequisite that cannot dispatch is a stall.
  - A switchover landing ahead of its claims plumbing is a silent loss.
  - This commit changes no Rust.

<!-- appendix -->
