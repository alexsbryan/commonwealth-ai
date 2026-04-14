# Getting Started with Commonwealth

This guide walks you through setting up a Commonwealth mesh with friends — from installing the software to running your first shared inference request. It assumes you're comfortable with a terminal but doesn't require deep systems knowledge.

---

## What You'll End Up With

By the end of this guide, you and your friends will have:

- A private mesh that pools everyone's GPU power and storage
- A shared AI model (e.g., Qwen3 70B) running across multiple machines — larger than any single machine could host alone
- A local API endpoint (`localhost:9741`) on each machine that any AI tool can use
- Automatic discovery, failover, and load balancing with zero configuration

---

## Step 1: Prerequisites

Each person needs:

1. **A computer with a GPU** — NVIDIA (CUDA), AMD (ROCm), or Apple Silicon (Metal). Even a MacBook Air contributes.
2. **llama.cpp installed** — Commonwealth orchestrates llama.cpp; it doesn't contain an inference engine itself.
3. **A network connection to the other machines** — either the same local network (Wi-Fi/Ethernet) or a VPN like Tailscale.

### Install llama.cpp

```bash
# macOS (Homebrew)
brew install llama.cpp

# Linux — build from source
git clone https://github.com/ggml-org/llama.cpp
cd llama.cpp
cmake -B build -DGGML_CUDA=ON   # or -DGGML_METAL=ON for Mac
cmake --build build --config Release
sudo cp build/bin/llama-server build/bin/rpc-server /usr/local/bin/
```

Verify it works:
```bash
llama-server --help
rpc-server --help
```

### Install Commonwealth

```bash
curl -sSf https://commonwealth.dev/install.sh | sh
```

Or build from source:
```bash
git clone https://github.com/commonwealth-rs/commonwealth
cd commonwealth
cargo build --release
sudo cp target/release/commonwealth /usr/local/bin/
```

Verify:
```bash
commonwealth --version
```

---

## Step 2: Network Setup

Commonwealth nodes need to be able to reach each other directly. There are two scenarios.

### Same Local Network (Easiest)

If everyone is on the same Wi-Fi or Ethernet network — a house, an office, a hackerspace — Commonwealth discovers peers automatically via mDNS. **No extra setup needed.** Skip to Step 3.

### Different Networks (Use Tailscale / Headscale)

If your friends are in different locations — or you're on a WiFi with client isolation enabled, which looks the same from Commonwealth's perspective — you need a VPN overlay so the machines can reach each other directly. [Tailscale](https://tailscale.com/) is the easiest; [Headscale](https://github.com/juanfont/headscale) is the self-hosted equivalent. Either works; the Commonwealth side is identical.

**On each machine:**

1. **Install Tailscale** (same client for both Tailscale cloud and Headscale):
   ```bash
   # macOS
   brew install tailscale

   # Linux (Debian/Ubuntu)
   curl -fsSL https://tailscale.com/install.sh | sh
   ```

2. **Join the tailnet:**

   **For Tailscale's hosted service:**
   ```bash
   sudo tailscale up
   ```
   Opens a browser to log in. Everyone should use the same Tailscale account (or use its sharing feature).

   **For self-hosted Headscale** — see [Appendix A](#appendix-a-headscale-setup) for server setup, then on each client:
   ```bash
   sudo tailscale up \
     --login-server https://headscale.your-domain.com \
     --authkey <your-preauth-key>
   ```

3. **Verify the tailnet is working:**
   ```bash
   tailscale status        # Shows all connected machines + their 100.x IPs
   tailscale ping <peer>   # Confirm direct connectivity
   ```

### Joining over Tailscale / Headscale

Commonwealth's discovery layer uses mDNS multicast, which Tailscale/Headscale **do not forward over the overlay** — that's an intentional WireGuard design choice, not a Commonwealth limitation. To join across a tailnet, append the founder's tailnet address to the join URL as a `?relay=` query parameter. The joiner tries the address directly before falling back to mDNS, so the hint is purely additive and doesn't change anything for on-LAN users.

**Workflow:**

1. **Founder** creates the mesh in the usual way and copies the join URL it generates. Looks like:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad
   ```

2. **Founder** grabs their tailnet address:
   ```bash
   tailscale ip -4
   # → 100.64.0.5
   ```
   Or the MagicDNS name:
   ```bash
   tailscale status | head -1
   # → machine-a.tailnet-abc123.ts.net
   ```

3. **Founder** appends `&relay=<address>` before sharing:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&relay=100.64.0.5
   ```
   Or with a hostname:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&relay=machine-a.tailnet-abc123.ts.net
   ```
   Port `9742` is the default — only append `:<port>` if you've changed the internal API bind.

4. **Joiner** pastes the modified URL into Settings → Mesh (desktop) or via the CLI:
   ```bash
   sovereign mesh join 'sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&relay=100.64.0.5'
   ```

**Gotchas:**

- **First-time firewall prompt on the founder.** macOS's application firewall will ask to allow the binary the first time a tailnet peer reaches `0.0.0.0:9742`. Allow it. If you dismissed the prompt, re-allow under System Settings → Network → Firewall → Options.
- **mDNS still runs locally.** Even with a `?relay=` hint, the joiner's daemon advertises on the LAN's `_commonwealth._tcp.local.` channel. If you happen to be on both the tailnet AND the same LAN, you may see the same peer twice in diagnostics — cosmetic, not a bug.
- **Headscale requires TLS-terminated connectivity to the coordination server**, not to the peers. Your actual mesh traffic still flows peer-to-peer over WireGuard; the Headscale server just tells nodes about each other.

---

## Step 3: Create the Mesh

**One person** creates the mesh:

```bash
commonwealth init --name "Sunset District Co-op"
```

Output:
```
Mesh created: Sunset District Co-op
Join key: cwth-7f3a-9b2e-4d1c

Share this key with people you want in the mesh.
They run: commonwealth join cwth-7f3a-9b2e-4d1c
```

**Share the join key** with your friends — text it, say it aloud, write it on a sticky note. The key is only used once during join. After that, authentication uses certificates exchanged during the handshake.

---

## Step 4: Join the Mesh

**Everyone else** runs:

```bash
commonwealth join cwth-7f3a-9b2e-4d1c
```

That's it. Commonwealth discovers each other's hardware automatically and starts coordinating.

Check the mesh:
```bash
commonwealth status
```

You should see all members, their GPUs, VRAM, and online status.

---

## Step 5: Download a Model

The mesh needs at least one model to serve. Everyone can run this, but only one copy needs to download — it transfers peer-to-peer to other nodes automatically.

```bash
# Download a model using llama.cpp's model downloader or huggingface-cli
huggingface-cli download Qwen/Qwen3-30B-GGUF qwen3-30b-q4_k_m.gguf \
  --local-dir ~/.commonwealth/models/
```

For the full "five neighbors" experience, a 70B model works best. If your mesh has 48+ GB of combined VRAM:
```bash
huggingface-cli download Qwen/Qwen3-72B-GGUF qwen3-72b-q4_k_m.gguf \
  --local-dir ~/.commonwealth/models/
```

---

## Step 6: Start the Daemon

On **every machine**:

```bash
commonwealth daemon start
```

Commonwealth will:
1. Detect your GPU, VRAM, CPU, and storage
2. Find other mesh members via mDNS or VPN gossip
3. Compute an optimal shard plan (which layers go on which GPU)
4. Start `llama-server` and `rpc-server` processes
5. Begin serving on `http://localhost:9741`

Check it's working:
```bash
commonwealth models       # Should show the loaded model
curl http://localhost:9741/status | python3 -m json.tool
```

---

## Step 7: Use It

### Quick test with curl

```bash
curl http://localhost:9741/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "Explain the concept of the commons in economics."}
    ]
  }'
```

You should get a response from the mesh's shared model — even if the model is too large to fit on your machine alone.

### With Open WebUI

[Open WebUI](https://github.com/open-webui/open-webui) gives you a ChatGPT-like interface to your mesh.

```bash
docker run -d -p 3000:8080 \
  -e OPENAI_API_BASE_URL=http://host.docker.internal:9741/v1 \
  -e OPENAI_API_KEY=not-needed \
  --name open-webui \
  ghcr.io/open-webui/open-webui:main
```

Open `http://localhost:3000` in your browser.

### With Sovereign

[Sovereign](https://github.com/pchaganti/gx-sovereign) is an agentic AI system designed to work with Commonwealth. It uses OICP to automatically select the right model for each step of a task.

In your Sovereign configuration:
```toml
[providers.mesh]
type = "remote_api"
url = "http://localhost:9741/v1"
```

Sovereign will:
- Poll `GET /oicp/v1/capabilities` to learn what models the mesh has
- Send OICP capability requirements per-request (e.g., `code: 3` for coding tasks, `analysis: 3` for research)
- Get routed to the best available model automatically

If your mesh has both a coding model (Qwen3-Coder) and a general model (Qwen3) loaded simultaneously, Sovereign's coding steps use the coder and research steps use the general model — without you configuring anything.

### With oMo or Other Coding Agents

Any tool that supports an OpenAI-compatible API endpoint can use Commonwealth. For [oMo](https://github.com/pchaganti/gx-omo) or similar coding agents:

```bash
# Set the API base to your Commonwealth mesh
export OPENAI_API_BASE=http://localhost:9741/v1
export OPENAI_API_KEY=not-needed

# Run your agent as usual — it talks to the mesh
omo "Refactor the authentication module"
```

The agent doesn't know it's talking to a distributed mesh. It just sees a fast, capable model at localhost.

---

## Step 8: Day-to-Day Operation

### Pausing your node

Going to bed? Running a game? Pause your contribution:
```bash
commonwealth pause
```
The mesh rebalances automatically. Other nodes pick up your layers. No requests are dropped.

Resume later:
```bash
commonwealth resume
```

### Checking the contribution balance

```bash
commonwealth balance
```
```
Sunset District Co-op — Contribution Balance (last 30 days)
──────────────────────────────────────────────────────────────
Node              Compute    Storage    Bandwidth    Balance
──────────────────────────────────────────────────────────────
Alice's Desktop    12.3h      0 GB       2.1 GB      +14.4
Bob's Build        18.7h      0 GB       1.8 GB      +20.5
Carol's Mac         8.6h     170 GB      48.2 GB     +52.8
Dave's Rig         22.1h      55 GB      12.4 GB     +34.5
Eve's MacBook Air   0.2h      0 GB       0.1 GB      -18.3
```

Carol contributes most via storage (hosting the knowledge index). Dave contributes most compute. Eve consumes more than she contributes — the ledger makes this visible. The group decides what to do about it (or not).

### Adding a new member

Just share the join key. New members discover the existing mesh and integrate automatically.

### Removing a member

```bash
commonwealth mesh revoke "Dave's Rig"
```
Requires majority vote from online members.

---

## Configuration Reference

Create `~/.commonwealth/config.toml`:

```toml
[node]
name = "Alice's Desktop"
data_dir = "~/.commonwealth"
api_port = 9741         # Client API port
internal_port = 9742    # Mesh-internal port

[contribution]
schedule = "always"     # "always" | "idle" | "manual"
reserve_vram_gb = 4     # Keep this much VRAM for local use
reserve_ram_gb = 8
reserve_storage_gb = 50

[inference]
llama_server = "/usr/local/bin/llama-server"
rpc_server = "/usr/local/bin/rpc-server"

[knowledge]
index_dir = "~/.commonwealth/indexes"

[fairness]
policy = { type = "transparent" }
```

Most fields have sensible defaults. The only required field is `[node] name`. Network-level configuration (Tailscale, Headscale, direct LAN) isn't expressed in this file — the daemon binds `0.0.0.0:9742` unconditionally, and cross-tailnet routing is supplied per-join via the `?relay=<address>` query parameter on the join URL. See Step 2 for the full workflow.

---

## Troubleshooting

**Nodes can't find each other (same WiFi)**
- Check that mDNS is not blocked by your firewall (port 5353 UDP)
- If the founder's logs show `mDNS service registered` but the joiner sees zero peers, and `ping <founder-ip>` also fails from the joiner, your router has **WiFi client isolation** (also called "AP isolation" or "guest isolation") enabled. It blocks both multicast and unicast between WiFi clients. Disable it in the router's wireless-advanced settings, or fall back to Tailscale — see Step 2's [Joining over Tailscale / Headscale](#different-networks-use-tailscale--headscale).
- `No route to host (os error 65)` in the handshake log is the same symptom at the TCP layer — same fix.
- First-time launch on macOS may silently drop incoming connections until the firewall prompt is answered. Re-allow via System Settings → Network → Firewall → Options.

**Nodes can't find each other (different networks)**
- Verify the overlay is up with `tailscale ping <peer>`
- Ensure the founder's join URL includes `&relay=<founder-tailnet-ip>` — without it, the joiner only tries mDNS, which doesn't cross the tailnet
- Check `commonwealth logs` for discovery messages

**Model won't load**
- Verify llama-server works standalone: `llama-server --model path/to/model.gguf`
- Check that combined VRAM across the mesh is sufficient for the model
- Check `commonwealth status` for scheduling errors

**Slow inference**
- Check `commonwealth status` — look at estimated TPS and which nodes host which layers
- High cross-node latency hurts: use `tailscale ping` to check RTT between nodes
- Consider loading a smaller quantization (Q4_K_M instead of Q8_0)

**"503 Service Unavailable" responses**
- A node probably just left the mesh. Wait 10-15 seconds for recovery and retry.
- The `Retry-After: 10` header tells clients when to retry.

---

## What's Next

- **Add knowledge bases**: Index Wikipedia, academic papers, or your own documents using Sovereign's corpus recipe engine, then serve them across the mesh
- **Peer with another mesh**: Connect two meshes for resource sharing with `commonwealth mesh peer <key>`
- **Run as a system service**: See `contrib/systemd/` (Linux) or `contrib/launchd/` (macOS) for service files
- **Load multiple models**: A coding model and a general model simultaneously — the mesh routes each request to the right one automatically

---

## Appendix A: Headscale Setup

[Headscale](https://github.com/juanfont/headscale) is an open-source, self-hosted replacement for Tailscale's coordination server. Using it means none of your mesh traffic or metadata touches Tailscale's infrastructure. The trade-off: one person in your group needs to run a small server with a public IP (a $5/month VPS works fine).

The key thing to understand: Headscale replaces only the *coordination server* (the thing that tells nodes about each other). The actual data still flows directly between your machines via WireGuard, just like with Tailscale. Your inference traffic never goes through the Headscale server.

### What one person does (the Headscale host)

You need a machine with a public IP address. A cheap VPS (Hetzner, DigitalOcean, etc.) works. This machine does NOT need a GPU — it's just a lightweight coordination service.

1. **Install Headscale:**
   ```bash
   # Download the latest release (check https://github.com/juanfont/headscale/releases)
   wget https://github.com/juanfont/headscale/releases/download/v0.25.1/headscale_0.25.1_linux_amd64.deb
   sudo dpkg -i headscale_0.25.1_linux_amd64.deb
   ```

2. **Configure it** — edit `/etc/headscale/config.yaml`:
   ```yaml
   # The URL others will use to reach this server.
   server_url: https://headscale.your-domain.com:443

   # Where Headscale listens.
   listen_addr: 0.0.0.0:443

   # Use your actual domain or IP address.
   # You'll need TLS — Let's Encrypt via Caddy/nginx is easiest.

   # The rest of the defaults are fine for a small mesh.
   ```

   > **Simplest TLS setup:** Put [Caddy](https://caddyserver.com/) in front of Headscale. Caddy handles Let's Encrypt certificates automatically.
   > ```
   > # Caddyfile
   > headscale.your-domain.com {
   >     reverse_proxy localhost:8080
   > }
   > ```
   > Then set `listen_addr: 0.0.0.0:8080` in Headscale's config and let Caddy handle port 443.

3. **Start Headscale and create a user:**
   ```bash
   sudo systemctl enable --now headscale

   # Create a user (namespace) for your mesh
   headscale users create commonwealth
   ```

4. **Create auth keys** for each person (or one reusable key):
   ```bash
   # One reusable key everyone can use
   headscale preauthkeys create --user commonwealth --reusable --expiration 720h
   # → outputs something like: 1234abcd5678efgh...
   ```

   Share this auth key with your friends alongside the Commonwealth join key.

### What everyone does (including the host)

Point the standard Tailscale client at your Headscale server and Commonwealth runs on top unchanged.

1. **Install Tailscale** (same as before — Headscale uses the stock Tailscale client):
   ```bash
   # macOS
   brew install tailscale

   # Linux
   curl -fsSL https://tailscale.com/install.sh | sh
   ```

2. **Connect to your Headscale server** (the `--login-server` flag is the only difference from vanilla Tailscale):
   ```bash
   sudo tailscale up \
     --login-server https://headscale.your-domain.com \
     --authkey 1234abcd5678efgh
   ```

3. **Verify connectivity:**
   ```bash
   tailscale status             # All Headscale peers + their 100.x IPs
   tailscale ping <peer-ip>     # RTT over the WireGuard tunnel
   ```

4. **Use the tailnet address in the join URL.** This is the entire integration point on the Commonwealth side. The founder reads their tailnet address:
   ```bash
   tailscale ip -4
   # → 100.64.0.5
   ```
   …and shares a join URL with `&relay=<that address>` appended:
   ```
   sovereign://join/cwth-d26f-cae1-65c6?name=Lab+Squad&relay=100.64.0.5
   ```
   Joiners use the modified URL in the desktop app (Settings → Mesh → paste) or via the CLI (`sovereign mesh join '<url>'`). Commonwealth's daemon never needs to know which coordination server you're using — it just needs an address to POST the handshake at.

That's the entire integration. From this point forward the rest of this guide applies unchanged regardless of which coordination server you run. See the [Joining over Tailscale / Headscale](#different-networks-use-tailscale--headscale) section in Step 2 for the full per-join workflow and the gotchas around macOS's application firewall.

### Headscale vs Tailscale: which to choose?

| | Tailscale | Headscale |
|---|---|---|
| **Setup time** | 5 minutes | 30-60 minutes |
| **Requires a server** | No | Yes (any cheap VPS) |
| **Cost** | Free tier (up to 100 devices) | VPS cost (~$5/month) |
| **Metadata privacy** | Tailscale sees which nodes connect | Fully self-hosted |
| **Data privacy** | Direct node-to-node (same either way) | Direct node-to-node (same either way) |
| **Maintenance** | None | Occasional updates |

For most groups, Tailscale is the right starting choice. Switch to Headscale later if metadata sovereignty matters to you — the migration is just re-running `tailscale up` with a different `--login-server`.
