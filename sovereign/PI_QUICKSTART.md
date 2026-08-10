# Use Pi with Commonwealth

Pi is a CLI coding agent. Commonwealth is a local LLM daemon that exposes an OpenAI-compatible API. Three steps to wire them up.

## 1. Start the daemon

```bash
svrn daemon start
svrn daemon status   # should say "daemon running"
```

If `sovereign` isn't on your PATH yet, build it:
```bash
cargo build -p sovereign-cli --release
ln -sf "$(realpath sovereign/target/release/sovereign-cli)" ~/.local/bin/svrn
```

## 2. Install Pi

```bash
npm install -g @earendil-works/pi-coding-agent
```

## 3. Point Pi at the daemon

```bash
bash scripts/setup-pi-provider.sh
```

Writes `~/.pi/agent/models.json` with a `commonwealth` provider at `http://localhost:9741/v1`. Idempotent — safe to re-run.

## Use it

```bash
cd ~/some/project
pi
```

Pick the `commonwealth` provider when prompted, then chat normally.

## Troubleshooting

**Pi says "no provider 'commonwealth'":** re-run `bash scripts/setup-pi-provider.sh`.

**Pi hangs on the first request:** check the daemon is up — `svrn daemon status`. If it's down, `svrn daemon start`.

**Daemon disappears mid-conversation (macOS):** the OS killed it for memory pressure. Use a smaller-quant model in `~/.svrnmesh/config.toml` (`Q4_K_M` instead of `Q6_K`) or lower `context_size` from 50000 to 16000. Then `svrn daemon restart`.

**Want to see what's happening:** logs live at `~/.svrnmesh/logs/daemon.err`.
