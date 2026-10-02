<!-- ledger -->

**phase-b-39 · 2026-09-28 · Phase B → pb-work-donor's four forks, answered by the seat so that nothing a user sees changes; the row is unparked · seat (operator's standing autonomy)**
- Needed: pb-work-donor parked operator-only at 11:14Z ("anything that changes end-user-observable behaviour beyond what a row states") with four forks. The seat checked the package's facts on this host. The two node ids differ (daemon 44ae…, cw-rails ac03…). The live offer is `process:v1` and `ingest:v1` with `yield_to_foreground = false`. No `rails.toml` exists.
- Chose (the seat, under the operator's autonomy grant, picking in each fork the option that changes no user-visible behaviour):
  1. The credit goes to the node's one roster identity. The daemon passes its NodeId at origin registration. After the flip that is cw-rails' own id, because pb-mesh-exit-transport keeps the daemon's key as the roster identity. The package's (a) would have moved all donated credit off the daemon's row.
  2. `ingest:v1` drops from the offer, with a warn, until an origin registers, through the existing drop-and-warn path. Today a down daemon means no ingest offer either.
  3. `Admit::Local` in OriginRegistry (never forwarded).
  4. The `[compute.work_offer]` migration rides the existing `hand_over` in sovereign-cli-mesh rail_migration.rs. `svrn mesh up` runs it (rails_up.rs:241), and it already moves `[iroh] media_viewer_user` out of svrn's config.toml (`migrate_viewer_key`, :195). No new door, and svrn never names cw-rails' data layout.
- Because: principle 11 (fork 4 reuses the handover that already migrates a config section), principle 8 (one roster identity for credit, converging with the flip), principle 12 (the origin's exposure is the registry's decision). The operator: do not sacrifice end-user quality. Delta restated from phase-b-31: donation runs only where cw-rails runs.
