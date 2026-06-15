# sovereign-mesh

Commonwealth mesh integration layer for Sovereign. Embeds the
Commonwealth daemon in-process, so mesh operations happen from within
the desktop app without a separate binary.

## Join flow

```
founder (machine A)                    joiner (machine B)
───────────────────                    ──────────────────
create_mesh(name)                      (idle)
  → generates cwth-XXXX-XXXX-XXXX
  → starts daemon on 0.0.0.0:9742
  → mDNS advertises _commonwealth._tcp
     with {node_id, mesh_id, name}
                                       user pastes sovereign://join/<key>?name=<>
                                       (or clicks an OS-registered link in release)
                                       ↓
                                       join_mesh(DeepLink)
                                         → validate_join_key_format
                                         → start daemon with placeholder mesh
                                           (mDNS advertises us now too)
                                         → perform_join:
                                           poll mdns.discovered_peers() up to 5s
                                           for entries with matching `name`
                                           ↓
                                       for each candidate:
                                         POST /internal/join { join_key,
                                                               joining_node_name,
                                                               addresses }
                                       ──────────────▶
                                                          hash(join_key) == stored?
                                                          yes → membership::accept_join
                                                                 adds MemberRecord
                                                                 returns MeshWire
                                                          no  → 401 (try next peer)
                                       ◀──────────────
                                       on 200: adopt MeshWire into local
                                       AppState. Gossip takes over from here.
```

## Ports

- `127.0.0.1:9741` — client-facing API (OpenAI-compatible, OICP, apps)
- `0.0.0.0:9742` — internal mesh API (`/internal/gossip`,
  `/internal/join`, knowledge fan-out). Bound to 0.0.0.0 so peers
  on the LAN can reach it; firewall gates apply.

## Testing two machines on the same LAN

1. `cargo tauri dev` on Machine A → Settings → Mesh → Create → name
   it. Copy the `sovereign://join/...` URL it shows.
2. `cargo tauri dev` on Machine B (same WiFi) → Settings → Mesh →
   paste the URL into the "Joining a friend's mesh?" input →
   Preview → Join.
3. Within ~5s the dialog resolves. Both machines' diagnostics
   panels now list the other under "Peers discovered via mDNS",
   and MeshStatus on both sides shows 2 members.

If step 3 hangs, enable tracing:

```sh
RUST_LOG=sovereign_mesh=debug,commonwealth_discovery=debug,commonwealth_api=debug \
  cargo tauri dev
```

Expected sequence on the joiner: `mDNS service registered` →
`discovered peer via mDNS` → `handshake_sent` →
`handshake_accepted`. Absence of `discovered peer` means mDNS is
blocked (firewall, VPN, different subnets).

## Joining over Tailscale / Headscale (or any overlay VPN)

Tailscale gives every tailnet peer a `100.x.x.x` address and
encrypted point-to-point routing, but it does **not forward UDP
multicast** — so our mDNS-based discovery fails silently across a
tailnet even though the HTTP route is reachable. Workaround: include
the founder's tailnet address in the join URL as a `?relay=` hint.
The joiner tries the hint directly before (and independently of) the
mDNS loop, and it remains a pure addition — if the hint doesn't
resolve, we still fall back to mDNS.

### Workflow

1. On Machine A, find its tailnet IP (`tailscale ip -4`) or the
   MagicDNS hostname (e.g. `machine-a.tailnet-abc123.ts.net`).
2. Create the mesh in the desktop app. You'll get a URL like:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=The+Masonics
   ```
3. **Append the address as a `relay` param** before sharing:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=The+Masonics&relay=100.64.0.5
   ```
   The port defaults to `9742` — append `:<port>` only if you've
   changed the internal API bind. Hostnames work too:
   ```
   …&relay=machine-a.tailnet-abc123.ts.net
   ```
4. On Machine B (also on the tailnet), paste the modified URL into
   Settings → Mesh. Within a couple of seconds you'll see
   `handshake_sent: direct-peer hint, POST /internal/join` followed
   by `handshake_accepted`.

### Gotchas

- **Firewall on Machine A.** The internal API binds `0.0.0.0:9742`,
  but macOS's application firewall will prompt to allow the binary
  the first time it's reached from a tailnet peer. Allow it.
- **`relay_hint` is a misleading name**, kept for URL-scheme stability
  (the `relay_hint` field in `DeepLink` is parsed by older builds
  too). Today it's treated as "try this peer directly" rather than
  "use this rendezvous service"; true relay discovery is still
  future work.
- **mDNS happens anyway.** Even with a `relay=` hint, mDNS starts up
  and will discover LAN peers. If you run one machine both on the
  tailnet and the LAN, you may see the same peer twice in the
  diagnostics panel — cosmetic, not a bug.

## Gossip

After the initial `/internal/join` handshake, each member runs a
periodic **push-pull gossip** loop (`src/gossip.rs`, spawned by
`EmbeddedDaemon::start_daemon`).

**Cadence:** every 10s by default. Each round:

1. Bump our own `last_seen` to `now`.
2. Pick up to 2 non-self members at random and POST our `Mesh`
   snapshot to their `/internal/gossip`.
3. Merge their reply into ours via `Mesh::merge_from` — per-member
   last-writer-wins by `last_seen`.
4. Mark any peer whose `last_seen` is older than 60s as `Offline`.

**Convergence:** pairwise in one round. For a 3+ member mesh, state
propagates transitively — anything that reaches one member appears
at every other member within a couple of rounds.

**Fast initial sync:** after `create_mesh`, `join_mesh`, or
`try_resume`, one gossip round fires immediately (bounded to 2s).
That's why a restart reconciles with the rest of the mesh in under
a couple of seconds instead of waiting a full interval.

**Auth boundary:** `/internal/gossip` rejects any incoming
`Mesh` whose `mesh_id` or `join_key_hash` doesn't match ours. Same
trust model as the join handshake — if you can spoof the
`join_key_hash`, you already have the join key and could just do a
real join.

**Not gossiped (yet):** capabilities, app state, knowledge-shard
plans, inference plans. The `commonwealth-discovery::GossipState`
scaffolding (three-phase Digest/Delta/Response protocol, KV layer)
is the intended home for those; for v1 we only gossip membership
via full-snapshot push-pull.

## Persistence

The running mesh is serialised to `<data_dir>/mesh.json` (macOS:
`~/Library/Application Support/sovereign/mesh.json`) after a
successful `create_mesh` or `join_mesh`. On app start, bootstrap
calls `EmbeddedDaemon::try_resume()` — if the file is present the
daemon starts again with that mesh, so mDNS advertises and joiners
can find you without you having to recreate. Clicking **Leave** in
Settings → Mesh deletes the file. The format is a flat JSON blob
(members as an array, not a HashMap — NodeId doesn't round-trip as
a JSON object key).

## Out of scope (v1)

- **True rendezvous-based discovery.** Tailscale works (see above)
  because the joiner already knows the founder's stable address.
  Bootstrapping across the public internet without a side channel
  (shared link + reachable host) still needs a rendezvous service
  — future work.
- **Plain HTTP on the join handshake.** The join_key is exposed on
  whatever network the handshake traverses (LAN, tailnet). Acceptable
  trust model for v1 ("shared via trusted chat"). Gossip + the internal
  API are plaintext today — the `TrustStore` / mutual-TLS scaffolding was
  unwired dead code and has been removed; transport security is future work.
- **Persistent mesh state across restarts.** `Mesh` lives in
  memory; quit the app and rejoin to rebuild.
- **CLI `sovereign mesh join <url>`.** The command works but its
  daemon exits immediately — the desktop app is the long-running
  participant. CLI is for scripting one-shot actions.
