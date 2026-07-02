# Running a mesh across networks

Commonwealth is the mesh inside Sovereign — you don't install or run it on its own. On a single network, pooling machines is just `sovereign mesh create` on one and `sovereign mesh join <key>` on the others; the [README](../../sovereign/README.md#commonwealth--pool-machines-with-people-you-trust) and [Run a model bigger than your machine](../../docs/RUN_A_BIGGER_MODEL.md) cover that path.

This guide is for the harder case: getting nodes to reach each other when they're on different networks, or on a LAN that blocks peer discovery. It's all about connectivity. Once the machines can see each other, you create and join the mesh exactly as you would locally.

## On the same network

If everyone is on the same Wi-Fi or Ethernet — a house, an office, a hackerspace — nodes find each other over mDNS, as long as your router isn't isolating clients. There's nothing to set up: create the mesh with `sovereign mesh create` and share the key. (If it says a mesh already exists, `sovereign setup` founded a solo one on first boot — read its key with `sovereign mesh status`, or mint a fresh one with `sovereign mesh rotate`.)

If discovery doesn't work on a network where you'd expect it to, the cause is almost always WiFi client isolation — see [Troubleshooting](#troubleshooting).

## Across different networks (no VPN needed)

If your friends are in different locations — or you're on a WiFi with client isolation, which looks the same to the mesh — you no longer need a VPN. Creating a mesh produces an invite that carries a **connect code**, and the joiner reaches the founder directly by that code over an encrypted, key-verified connection, wherever the two machines are. There is nothing to install beyond Sovereign itself.

The steps are the same as on a LAN, with one thing to get right: **share the full invite link, not just the bare key.** The link carries the connect code; a bare `cwth-…-…-…` key only works on the same network (via local discovery).

1. The founder creates the mesh and copies the invite. The desktop app's share card and `sovereign mesh create` both produce a link that already includes the connect code — for example:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&dial=7f3a…@…
   ```
   (The `dial=` part is the connect code. It appears once the founder's node has learned a reachable address — a second or two after create; the share card refreshes itself.)

2. The joiner uses that full link — in the desktop app under Settings → Mesh (it shows "Connects directly — no VPN needed"), or on the CLI:
   ```bash
   sovereign mesh join 'sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&dial=7f3a…@…'
   ```

The joiner dials the founder by key first; if that path is momentarily unavailable it falls back to local discovery automatically, so the same link works whether the joiner is across the world or in the same room.

**How it reaches across NAT.** A public *relay* introduces two nodes that are both behind home routers and helps them find a direct path; once found, traffic flows node-to-node. No mesh data — no prompts, no model output, no corpus — is stored on or readable by the relay; it only passes encrypted packets. By default this uses the [iroh](https://www.iroh.computer/) project's public relays, plus iroh's public DNS service to help peers find each other's addresses.

To keep everything on infrastructure you control, there are two levels:

```toml
# ~/.sovereign/config.toml
[iroh]
enabled = true
# Level 1 — your own relay, but still use iroh's public address-lookup:
relay_urls = ["https://relay.your-domain.com:443"]
# Level 2 — sever ALL contact with iroh's public services. Peers are then
# found only via your relay and gossiped addresses, nothing phones home:
discovery = "none"
```

Note the distinction: `relay_urls` alone moves the *relay* to your box but peers still use iroh's public DNS to publish and resolve addresses. `discovery = "none"` is what a security team means by "no third party" — on a flat LAN/VPC it needs no relay at all (gossiped addresses suffice); across subnets, pair it with your own `relay_urls`. Both settings are per-node and gossiped, so you migrate machines one at a time and a mixed set interoperates. This is the path for **air-gapped or multi-site fleets** and **corporate networks that block the public relay domains** — see [ENTERPRISE_FLEET_DEPLOY.md](../../sovereign/docs/ENTERPRISE_FLEET_DEPLOY.md).

**Corporate / locked-down networks.** The relay path is TCP over port 443 (WebSocket-over-TLS), so it works even where UDP is blocked outright — the same worst-case fallback a VPN relies on. If your network requires an HTTP proxy, set `HTTP_PROXY` / `HTTPS_PROXY` in the daemon's environment and the relay connection honors it.

A couple of things to know:

- On macOS, the first time a peer reaches this node its firewall may ask to allow the binary; allow it. (Re-enable under System Settings → Network → Firewall → Options if you dismissed it.)
- Local discovery (mDNS) still runs on the LAN. On the same network you don't need the connect code at all — the bare key works.

Once the machines can reach each other, create and join the mesh the usual way, and the host's model serves everyone — [Run a model bigger than your machine](../../docs/RUN_A_BIGGER_MODEL.md) walks through that side.

## Prefer your own VPN overlay? (Tailscale or Headscale)

Running the mesh over a VPN overlay is now optional, but still fully supported — some groups already run [Tailscale](https://tailscale.com/) or self-hosted [Headscale](https://github.com/juanfont/headscale), or prefer a WireGuard mesh they manage themselves. It's also the simplest answer for **multi-host distributed inference**, where a model is split across several GPU machines: that layer speaks raw TCP between the boxes and needs them on a shared IP network (a LAN, a VPC, or a VPN overlay) regardless of transport.

On a VPN overlay the machines get stable addresses but mDNS doesn't cross the tunnel, so pass the founder's overlay address as a `?relay=` hint on the join URL (the joiner tries it before falling back to local discovery):

```bash
# founder reads their overlay address, e.g. Tailscale:
tailscale ip -4          # → 100.64.0.5
# then shares:
sovereign mesh join 'sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&relay=100.64.0.5'
```

Port `9742` is the default internal port; only add `:<port>` if you've changed the bind. Self-hosting the coordination server with Headscale keeps all metadata off Tailscale's infrastructure — see the [appendix](#appendix-self-hosting-with-headscale).

## Troubleshooting

**Nodes can't find each other on the same WiFi.**

- Check that mDNS isn't blocked by a firewall (UDP port 5353).
- If the founder's logs show `mDNS service registered` but the joiner sees zero peers, and `ping <founder-ip>` also fails, your router has WiFi client isolation (sometimes called AP or guest isolation) turned on. It blocks traffic between WiFi clients. Turn it off in the router's wireless-advanced settings, or just join with the connect code — that path doesn't need the LAN at all. See [Across different networks](#across-different-networks-no-vpn-needed).
- `No route to host (os error 65)` during the handshake is the same problem at the TCP layer, with the same fix.
- On macOS, the first launch can drop incoming connections until you answer the firewall prompt. Re-allow under System Settings → Network → Firewall → Options.

**Nodes can't find each other across networks.**

- Make sure the joiner used the **full invite link** (the one containing `dial=`), not the bare `cwth-…` key. The bare key only works on the same network.
- If the founder's invite shows no `dial=` yet, its node hasn't learned a reachable address — wait a moment and re-copy the link (the share card refreshes), or check that `[iroh] enabled` isn't forced off.
- If your network blocks the public relay domains, run your own relay and set `relay_urls` (see [Across different networks](#across-different-networks-no-vpn-needed)).
- Check `sovereign mesh status` to see who the node thinks it's connected to.
- Using a VPN overlay instead? Confirm it's up (e.g. `tailscale ping <peer>`) and that the join URL includes `&relay=<founder-overlay-ip>`.

**Inference feels slow.** Cross-node latency is the usual culprit; a model split across a slow link pays for it at every layer boundary. Check the round-trip between machines (`ping <peer>`, or `tailscale ping` on an overlay).

**"503 Service Unavailable".** A node probably just left the mesh. Wait ten or fifteen seconds for it to recover, then retry; the `Retry-After` header says how long.

## Appendix: self-hosting with Headscale

[Headscale](https://github.com/juanfont/headscale) is an open-source, self-hosted replacement for Tailscale's coordination server. Running it means none of your mesh traffic or metadata touches Tailscale's infrastructure. The trade-off is that one person runs a small server with a public IP — a cheap VPS is enough, and it needs no GPU.

Headscale replaces only the coordination server, the part that tells nodes about each other. The data still flows directly between machines over WireGuard, the same as with Tailscale; inference traffic never passes through the Headscale server.

The one person running Headscale:

1. Install it on a machine with a public IP:
   ```bash
   # check https://github.com/juanfont/headscale/releases for the latest
   wget https://github.com/juanfont/headscale/releases/download/v0.25.1/headscale_0.25.1_linux_amd64.deb
   sudo dpkg -i headscale_0.25.1_linux_amd64.deb
   ```

2. Configure `/etc/headscale/config.yaml`: set `server_url` to the address others reach it at, and `listen_addr`. You'll need TLS — the simplest path is to put [Caddy](https://caddyserver.com/) in front, which handles Let's Encrypt automatically:
   ```
   # Caddyfile
   headscale.your-domain.com {
       reverse_proxy localhost:8080
   }
   ```
   Then set `listen_addr: 0.0.0.0:8080` and let Caddy own port 443.

3. Start it, and create a user and a reusable auth key:
   ```bash
   sudo systemctl enable --now headscale
   headscale users create commonwealth
   headscale preauthkeys create --user commonwealth --reusable --expiration 720h
   ```
   Share that auth key with the group alongside the mesh join key.

Everyone, including the host, points the stock Tailscale client at the Headscale server — the `--login-server` flag is the only difference from vanilla Tailscale:

```bash
sudo tailscale up \
  --login-server https://headscale.your-domain.com \
  --authkey <the-preauth-key>
```

From there it's the same as Tailscale: read your tailnet address with `tailscale ip -4`, append `&relay=<address>` to the join URL, and joiners use it as in [Prefer your own VPN overlay?](#prefer-your-own-vpn-overlay-tailscale-or-headscale).

### Tailscale or Headscale?

| | Tailscale | Headscale |
|---|---|---|
| Setup | about 5 minutes | 30–60 minutes |
| Needs a server | no | yes (any cheap VPS) |
| Cost | free up to 100 devices | ~$5/month for the VPS |
| Metadata | Tailscale sees which nodes connect | fully self-hosted |
| Data path | direct, node to node | direct, node to node |

Most groups don't need either — the no-VPN connect code above covers joining across networks. Reach for an overlay when you want your own managed network, or for multi-host distributed inference (which needs the GPU machines on a shared IP network). Between the two, Tailscale is the quicker start; move to Headscale if metadata sovereignty matters — the switch is just re-running `tailscale up` with a different `--login-server`.
