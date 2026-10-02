# NEEDS_HUMAN — fp-20 blocked on a false premise, operator decision required

## (a) Unit

`fp-20` (row left `[~]` in `ralph/next/five-programs/STATE.md`):

> REPOINT the daemon's media routes to cw-rails (TSV sovereign-daemon→commonwealth-media,
> 33 refs; the routes are ALREADY served — /v1/mesh/media, /v1/mesh/media/fanout,
> /v1/mesh/publish — nothing new to build): DA media_presence.rs:109, publish_http.rs:33,
> media_reach.rs:44, origin_fanout.rs:50 become client stubs; absence reported.
> — check: scoped lint 0; daemon Cargo.toml drops commonwealth-media; gate drops 1

No edits were made to any source file. The tree is exactly as this session found it
(clean at 0fe0f31be). The row's premise is true for five route families and false for
two of the four anchored files; the row's own check (drop the Cargo dep) cannot be met
without building capability the premise says already exists.

## (b) What was run and what it actually printed

**Ref census** — `git grep -c commonwealth_media -- sovereign/crates/sovereign-daemon`:

    daemon.rs:3  internal_principal.rs:2  media_presence.rs:4  media_reach.rs:2
    origin_fanout.rs:1  publish_http.rs:2                     (= 14 in src/)
    tests/main/capabilities_published.rs:6  gossip_integration/media.rs:9
    gossip_offer_clock.rs:2  iroh_dialer_admission_e2e.rs:2   (= 19 in tests/)

33 total — matches the TSV. `commonwealth-media` sits under `[dependencies]`
(sovereign/crates/sovereign-daemon/Cargo.toml:21), so the 19 test refs block the dep
drop exactly as much as the src refs do.

**cw-rails route census** — the premise's three families ARE served, byte-identical
handlers (`commonwealth/crates/commonwealth-rails/src/api.rs:41-59`):
`/v1/mesh/media` (get :44), `/v1/mesh/app` (get :45), `/v1/mesh/fanout` +
`/v1/mesh/media/fanout` (post :48-49), `/v1/mesh/publish` ×4 (:52-57). But:

    $ git grep -n "v1/mesh/offers" -- commonwealth/crates/commonwealth-rails/src
    (no output — no offers mount)

**Presence at cw-rails** — `git grep -rni "presence|media_available|Sessions" --
commonwealth/crates/commonwealth-rails/` returns ONE hit:

    commonwealth/crates/commonwealth-rails/src/gossip.rs:79:        media_available: None,

No presence route, no poll, no reader of the origin. The gossip field exists and is
written by nothing. (The decision function `commonwealth_media::presence::
media_available_from_sessions` is decision-only — "Nothing here dials anything",
commonwealth/crates/commonwealth-media/src/presence.rs:9 — so there is no poll loop
at the target to reuse either. The poll loop is daemon code:
sovereign/crates/sovereign-daemon/src/media_presence.rs `run`/`read_presence`/`report`.)

**The offers route is live** — mounted at sovereign/crates/sovereign-daemon/src/
mesh_http.rs:54 (`/v1/mesh/offers` → `media_reach::mesh_offers`), driven by
sovereign/crates/sovereign-cli-mesh/src/mesh_offers.rs:489 and :550. cw-rails'
origin handler (api.rs:165) is already kind-generic over `OriginKind` — the mount
would be one line — but today it is not mounted.

**Boundary gate** — `RALPH_QUEUE=five-programs scripts/ralph-check.sh boundary`:

    boundary-gate FAILED (78 violation(s))

(78 is the honest pre-row count; queue-repair commit 0fe0f31be quotes the same.)

**Peer check** — `work_in_flight(scope="sovereign/crates/sovereign-daemon/src",
match_mode="file")`: no claims, no observations.

## (c) What the operator must decide

1. **The presence poll has no cw-rails surface to dial.** media_presence.rs:109 asks
   the media ORIGIN (Jellyfin `/Sessions`) with the house credential and decides via
   `commonwealth_media`. "Repoint to cw-rails" needs rails to serve a presence
   reading; nothing there does. Options:
   (a) mint a rails-side row FIRST: cw-rails runs the poll (it may legally dep
   commonwealth-media) and writes its own `media_available` gossip field
   (gossip.rs:79) — note the poll's credential plumbing (`MediaRoute::house()`,
   viewer/house discipline) lives in `sovereign_mesh::iroh_access`, which cmnwlth
   cannot depend on, so the rails-side poll re-derives origin + house credential
   from rails' own config (`Config.media.origin`) and
   `commonwealth_media::house_dir_under`. Real design work, operator's call.
   (b) rescope fp-20 to leave the poll in the daemon and close only the routes half
   — but then the Cargo dep cannot drop and the gate does not move; the row's check
   is not meetable and the row must say so.
   (c) something else.

2. **`GET /v1/mesh/offers` is not mounted at cw-rails.** Proxied like its siblings it
   needs a one-line mount in rails' router (api.rs:41-59) — a route-level addition
   the row's "nothing new to build" premise denies. Decide whether that mount lands
   in fp-20 or in a prior rails-side row.

3. **`internal_principal.rs:80` `MemberIdentity`** — `member_by_pubkey` (:150)
   returns it; it is acceptor identity plumbing, not a route, and has no dial shape.
   It needs a home the daemon may name once the dep drops (sovereign-contracts move
   per D3? a sovereign-mesh re-export — sovereign-mesh already legally deps
   commonwealth-media? kept, keeping the edge?). The row is silent.

4. **19 of the 33 refs are tests of the very path this row deletes.**
   capabilities_published.rs (6), gossip_integration/media.rs (9) and
   gossip_offer_clock.rs (2) run daemon gossip rounds and use
   `commonwealth_media::offers` as the read-oracle over the embedded fabric's mesh;
   iroh_dialer_admission_e2e.rs:396,511 construct the daemon with
   `PublishedApps::with_config/default` — the ledger field the dial removes. They
   build `sovereign_daemon::state::AppState`, so D6's move-beside-the-owner pattern
   does not fit verbatim, and they cannot dial a rails process in unit tests.
   Decide: rewrite against the proxy as e2e, move the offer-projection half beside
   commonwealth-media over a fabric-less roster fixture, or delete with the path.

5. FYI, not blocking: proxied routes answer from cw-rails' mesh view, not the
   daemon's still-embedded fabric, until the fp-core/mesh-dial rows land — that is
   D2's own delta ("media presence/fanout served by cw-rails; cw-rails required"),
   stated so it is chosen rather than discovered.

Suggested shape if you want the mechanical half to move now: a rails-side row
(mount `/v1/mesh/offers`; stand up the presence poll writing gossip.rs's field;
home for `MemberIdentity`), then fp-20 becomes a pure proxy-and-delete that meets
every check it names.

## (d)

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`
