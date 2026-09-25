# NEEDS_HUMAN — REVIEW-mint-fp-guest-attest (premise false, pre-mint)

## (a) The unit

`- [~] REVIEW-mint-fp-guest-attest — depends [] — REVIEW-MINT the guest-write re-mount (operator answer HUMAN-fp54 = (b), 2026-09-24): the daemon signs a per-guest-session attestation with its node key (the key cw-rails already trusts through the shared load_or_generate_node_key), its wire shape sits in commonwealth-rail-core beside SignedOp, and cw-rails verifies it before honouring on_behalf_of ... cap 4.`

No rows minted. The design the rows would carry hangs on the parenthetical, and the parenthetical does not hold in the tree.

## (b) What I measured

The loader is shared, the key is not. Both processes call `commonwealth_transport::identity::load_or_generate_node_key`, but on different directories, so on a default install they hold two different keys.

- Daemon: `load_or_generate_node_key(&self.data_dir)` (sovereign-daemon/src/daemon.rs:1291, :1614, :3072); `data_dir` is `svrnmesh_root()` (sovereign-contracts/src/rebrand.rs:106), i.e. `~/.svrnmesh`.
- cw-rails: `load_or_generate_node_key(&data_dir)` (commonwealth-rails/src/lib.rs:140), where `data_dir = Config::resolve_data_dir(..)` (cli.rs:150): `--data-dir`, else `$CW_RAILS_DIR`, else `~/.commonwealth-rails` (commonwealth-media/src/declared.rs:117-125).
- Nothing launches rails pointed at the svrnmesh dir. The fp-70 census established this (ctl/NEEDS_HUMAN.resolved-fp70-20260924.md:33; DECISIONS five-programs-33), and `git grep CW_RAILS_DIR` still hits only rails, commonwealth-media, rail_migration.rs and one test.
- On this host:
  ```
  $ ls -la ~/.svrnmesh/node_key ~/.commonwealth-rails/node_key
  -rw-------. 1 alexbryan alexbryan 32 Jun 15 11:52 /home/alexbryan/.svrnmesh/node_key
  ls: cannot access '/home/alexbryan/.commonwealth-rails/node_key': No such file or directory
  ```
  rails' first boot here will therefore generate a FRESH key, not the daemon's.
- rails' ring roster is `derive_roster(mesh, self_id, self_pubkey)` over rails' OWN mesh.json (commonwealth-rails/src/rail.rs:77-87, lib.rs:237). The daemon's pubkey is in that roster only if the daemon is a member of the mesh rails joined. No config, file or code path gives rails the daemon's pubkey as a trusted attester.

The same false premise sits under two recorded claims. fp-54's "the signer identity does not change" (commonwealth-rails/src/rail.rs:19-25, lib.rs:225-231) and decision five-programs-16's evidence bullet ("rails and the daemon both derive the node key from ... That is why option (b) ... needs no session state") both treat the shared loader as a shared key. So on a default install, since 24070aeb8, rails signs ring appends with a key the daemon's rosters have never seen. Whether those appends then fail `NotInRoster` depends on whether rails has joined the same mesh as its own member. I did not run it. This is a finding for fp-54, which is already closed, and it is outside this row.

## (c) What the operator must decide

1. **Whose key does rails trust as the attester?** Each option below is a real design, and they differ in the rows they need:
   - (i) **One node identity for both processes.** rails loads the daemon's `node_key`, either through a launcher that sets `--data-dir`/`CW_RAILS_DIR` or through a rails config key naming the key path. This makes the row's premise true, and it makes fp-54's "signer identity does not change" true too. The attestation then verifies against rails' own `node.pubkey()` with no new trust config. Cost: it touches §4 rule 1 ("one data directory, one owner"; rail_migration.rs:7-10) unless the key alone is shared by path.
   - (ii) **Any roster member of the namespace may attest.** rails verifies the attestation's signer against the roster it already derives (rail.rs:77). No new config. This matches `SignedOp::on_behalf_of`'s documented meaning ("this door says so", rail-core lib.rs:373-381). It needs the daemon to be a member of rails' mesh, and it widens attesters to every member.
   - (iii) **A pinned attester pubkey in rails' config** (e.g. `[rail] guest_attester = "<hex>"`), set by setup or the operator. The trust is explicit and narrow. It needs a new config key, a setup step, and an absence report when the key is unset.
2. **Is the fp-54 signer-identity gap its own row?** It is independent of guest writes: it affects every ring append on a default install. Reviewer lean: yes, and ahead of this mint, because option (i) closes both.

Measured row count once 1 is decided: 3 rows under option (ii) or (iii), and 4 under option (i) (the identity row first). All fit cap 4. The three common rows are:
- (1) rail-core `GuestAttestation` wire type beside `SignedOp`, with sign/verify over canonical bytes that carry name, namespace, session expiry and signer, plus tests.
- (2) cw-rails verifies it in `append_act` (rail.rs:209-229) and honours `on_behalf_of`, refusing forged, expired and foreign-key attestations with named statuses.
- (3) the daemon issues the attestation per guest session, sends it through `rails_client::journal_append` (rails_client.rs:400), and deletes the 503 (routes_rail.rs:414-431).

Each row carries the three tests the mint row names.

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
