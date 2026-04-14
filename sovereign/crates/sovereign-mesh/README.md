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

## Out of scope (v1)

- **Off-LAN / relay-based discovery.** `relay_hint` is parsed from
  the deep link but goes nowhere today. Needs a rendezvous service.
- **Plain HTTP on the join handshake.** The join_key is exposed on
  the LAN during the handshake. Acceptable trust model for v1
  ("shared via trusted chat"). Gossip uses mutual TLS post-handshake
  via the existing `TrustStore`.
- **Persistent mesh state across restarts.** `Mesh` lives in
  memory; quit the app and rejoin to rebuild.
- **CLI `sovereign mesh join <url>`.** The command works but its
  daemon exits immediately — the desktop app is the long-running
  participant. CLI is for scripting one-shot actions.
