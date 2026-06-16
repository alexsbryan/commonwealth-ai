# Running a mesh across networks

Commonwealth is the mesh inside Sovereign — you don't install or run it on its own. On a single network, pooling machines is just `sovereign mesh create` on one and `sovereign mesh join <key>` on the others; the [README](../../sovereign/README.md#commonwealth--pool-machines-with-people-you-trust) and [Run a model bigger than your machine](../../docs/RUN_A_BIGGER_MODEL.md) cover that path.

This guide is for the harder case: getting nodes to reach each other when they're on different networks, or on a LAN that blocks peer discovery. It's all about connectivity. Once the machines can see each other, you create and join the mesh exactly as you would locally.

## On the same network

If everyone is on the same Wi-Fi or Ethernet — a house, an office, a hackerspace — nodes find each other over mDNS, as long as your router isn't isolating clients. There's nothing to set up: create the mesh with `sovereign mesh create` and share the key.

If discovery doesn't work on a network where you'd expect it to, the cause is almost always WiFi client isolation — see [Troubleshooting](#troubleshooting).

## Across different networks (Tailscale or Headscale)

If your friends are in different locations — or you're on a WiFi with client isolation, which looks the same to the mesh — you need a VPN overlay so the machines can reach each other directly. [Tailscale](https://tailscale.com/) is the easiest; [Headscale](https://github.com/juanfont/headscale) is the self-hosted equivalent, and the mesh side is identical either way.

On each machine, install Tailscale:

```bash
# macOS
brew install tailscale

# Linux (Debian/Ubuntu)
curl -fsSL https://tailscale.com/install.sh | sh
```

Then join the tailnet. For Tailscale's hosted service:

```bash
sudo tailscale up
```

That opens a browser to log in; everyone should use the same account, or Tailscale's sharing feature. For self-hosted Headscale, see the [appendix](#appendix-self-hosting-with-headscale). Confirm it's working:

```bash
tailscale status        # all connected machines and their 100.x addresses
tailscale ping <peer>   # direct connectivity
```

## Joining across a tailnet

The mesh's discovery uses mDNS multicast, which Tailscale and Headscale don't forward over the overlay — a WireGuard design choice, not a mesh limitation. To join across a tailnet, add the founder's tailnet address to the join URL as a `?relay=` parameter. The joiner tries that address directly before falling back to mDNS, so the hint is purely additive and changes nothing for people on the LAN.

1. The founder creates the mesh and copies the join URL, which looks like:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad
   ```

2. The founder reads their tailnet address:
   ```bash
   tailscale ip -4
   # → 100.64.0.5
   ```
   or the MagicDNS name from `tailscale status`.

3. The founder appends `&relay=<address>` before sharing:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&relay=100.64.0.5
   ```
   Port `9742` is the default; only add `:<port>` if you've changed the internal bind.

4. The joiner uses the modified URL — in the desktop app under Settings → Mesh, or on the CLI:
   ```bash
   sovereign mesh join 'sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&relay=100.64.0.5'
   ```

A few things to know:

- The first time a tailnet peer reaches the founder, macOS's firewall asks to allow the binary on `0.0.0.0:9742`. Allow it; if you dismissed the prompt, re-enable it under System Settings → Network → Firewall → Options.
- mDNS still runs on the local network. If you're on both the tailnet and the same LAN, a peer may show up twice in diagnostics — cosmetic, not a bug.
- Headscale needs TLS to reach its coordination server, not to the peers. Mesh traffic still flows directly between machines over WireGuard; the server only tells nodes about each other.

Once the machines can reach each other, create and join the mesh the usual way, and the host's model serves everyone — [Run a model bigger than your machine](../../docs/RUN_A_BIGGER_MODEL.md) walks through that side.

## Troubleshooting

**Nodes can't find each other on the same WiFi.**

- Check that mDNS isn't blocked by a firewall (UDP port 5353).
- If the founder's logs show `mDNS service registered` but the joiner sees zero peers, and `ping <founder-ip>` also fails, your router has WiFi client isolation (sometimes called AP or guest isolation) turned on. It blocks traffic between WiFi clients. Turn it off in the router's wireless-advanced settings, or use Tailscale instead — see [Across different networks](#across-different-networks-tailscale-or-headscale).
- `No route to host (os error 65)` during the handshake is the same problem at the TCP layer, with the same fix.
- On macOS, the first launch can drop incoming connections until you answer the firewall prompt. Re-allow under System Settings → Network → Firewall → Options.

**Nodes can't find each other across networks.**

- Confirm the overlay is up with `tailscale ping <peer>`.
- Make sure the join URL includes `&relay=<founder-tailnet-ip>`; without it the joiner only tries mDNS, which doesn't cross the tailnet.
- Check `sovereign mesh status` to see who the node thinks it's connected to.

**Inference feels slow.** Cross-node latency is the usual culprit. Check the round-trip between machines with `tailscale ping <peer>`; a model split across a slow link pays for it at every layer boundary.

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

From there it's the same as Tailscale: read your tailnet address with `tailscale ip -4`, append `&relay=<address>` to the join URL, and joiners use it as in [Joining across a tailnet](#joining-across-a-tailnet).

### Tailscale or Headscale?

| | Tailscale | Headscale |
|---|---|---|
| Setup | about 5 minutes | 30–60 minutes |
| Needs a server | no | yes (any cheap VPS) |
| Cost | free up to 100 devices | ~$5/month for the VPS |
| Metadata | Tailscale sees which nodes connect | fully self-hosted |
| Data path | direct, node to node | direct, node to node |

For most groups Tailscale is the right starting point. Move to Headscale later if metadata sovereignty matters — the switch is just re-running `tailscale up` with a different `--login-server`.
