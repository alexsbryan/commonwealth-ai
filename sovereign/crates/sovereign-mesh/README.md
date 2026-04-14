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

## Out of scope (v1)

- **True rendezvous-based discovery.** Tailscale works (see above)
  because the joiner already knows the founder's stable address.
  Bootstrapping across the public internet without a side channel
  (shared link + reachable host) still needs a rendezvous service
  — future work.
- **Plain HTTP on the join handshake.** The join_key is exposed on
  whatever network the handshake traverses (LAN, tailnet). Acceptable
  trust model for v1 ("shared via trusted chat"). Gossip uses mutual
  TLS post-handshake via the existing `TrustStore`.
- **Persistent mesh state across restarts.** `Mesh` lives in
  memory; quit the app and rejoin to rebuild.
- **CLI `sovereign mesh join <url>`.** The command works but its
  daemon exits immediately — the desktop app is the long-running
  participant. CLI is for scripting one-shot actions.
