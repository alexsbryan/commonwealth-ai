# Troubleshooting

> **Using the desktop app rather than maintaining it?** Read
> [HAVING_TROUBLE.md](HAVING_TROUBLE.md) instead. It covers the same
> failures without a terminal, and it's the page to hand to someone you
> onboarded. Everything below assumes a shell, a service manager, and
> that `rm -rf` on the wrong path is your problem to recover from.

Common issues with `sovereign setup`, the daemon, and per-project code intelligence. Run [`sovereign doctor`](CLI_REFERENCE.md#sovereign-doctor) first — it covers most of these automatically. When in doubt, re-running `sovereign setup --reset` is safe.

← [back to README](../README.md)

## Setup / daemon

### `sovereign setup` finishes but `waiting for daemon to come up` times out

The daemon is crash-looping, usually because a downloaded GGUF is corrupt or the model format isn't supported by the bundled `llama.cpp`. Diagnose:

```sh
# macOS
launchctl list | grep sovereign
tail -f ~/.svrnmesh/logs/daemon.err

# Linux
systemctl --user status sovereign
journalctl --user -u sovereign -f
```

If the log shows `Failed to load model: null result from llama cpp`, the GGUF file is invalid. Check its size:

```sh
ls -la ~/.svrnmesh/models/
```

Anything under ~100 MB is almost certainly an HTML error page from a failed Hugging Face download. Fix:

```sh
rm -rf ~/.svrnmesh/models/*
sovereign setup --reset
```

### `sovereign setup` says "Already set up"

A config file exists at `~/.svrnmesh/config.toml`. Use `sovereign setup --reset` to wipe and reconfigure, or edit the file manually. (Pre-consolidation installs that wrote to the XDG config dir are migrated automatically on first load.)

### Want to switch models after setup

Edit the `[models]` section of the config file directly, then restart the daemon:

```sh
sovereign daemon restart
```

Or run `sovereign setup --reset` for a full re-download.

### Daemon is running but `:9741` returns connection refused

Check the actual listening port:

```sh
lsof -iTCP -sTCP:LISTEN -P | grep sovereign
```

If it's listening on a different port, your `~/.svrnmesh/config.toml` has a non-default `client_port` — edit and restart. If nothing is listening, the daemon didn't start cleanly; see the first entry above.

### Port conflict with another tool

Many devtools grab `:8080` or `:3000`; `:9741` was chosen to avoid that. If something else owns `:9741`:

```toml
# ~/.svrnmesh/config.toml
[daemon]
client_port = 19741
internal_port = 19742
```

Restart the daemon. Update any `.opencode/opencode.json` / `.mcp.json` that reference the old port (re-run `sovereign project init` and it'll pick up the new port).

## Project init

### `sovereign project init` shows "unknown flag" for `--help`

You're running a stale binary. Rebuild:

```sh
cd /path/to/sovereign
cargo build --release -p sovereign-cli
```

Every subcommand (including sub-subcommands like `project init`) recognises `--help` in the current binary.

### `sovereign project serve` listens on `:8080` instead of `:9741`

Also a stale binary. Verify:

```sh
sovereign project serve --help | grep port
# should say: --port <port>   Listen port (default: 9741)
```

If the default is 8080, rebuild. See [CLI_REFERENCE.md](CLI_REFERENCE.md) for the current flag list.

### MCP tools are missing after `project init`

Check that the server can see the index:

```sh
sovereign project status
curl -s http://localhost:9741/mcp/stats
```

If `project status` shows the index but `/mcp/stats` returns nothing, the daemon is running a stale build. Restart it (see the daemon section above).

## Mesh

### `sovereign mesh create` fails with "mesh already exists"

`sovereign setup` silently creates a solo mesh so the daemon has state to resume. To get a new shareable key:

```sh
sovereign mesh rotate
```

This generates a new plaintext key, updates the persisted hash, and prints the share URL. Existing members stay connected — only future joins need the new key. If joiners refuse a freshly rotated key, rotate in the daemon directly: `curl -s -X POST http://localhost:9741/v1/mesh/rotate` (the CLI and daemon can disagree about the mesh state dir — known issue, 2026-07). The invite/join/port mechanics themselves live in [join a mesh](../../docs/JOIN_A_MESH.md).

### Friend's `mesh join` hangs

Two common causes:

1. **mDNS blocked** — Router or AP isolation. Add a `?relay=<your-ip>:9742` query param: `sovereign mesh join sovereign://join/<key>?relay=192.168.1.100`.
2. **Firewall** — macOS prompts for incoming connection permission the first time; Linux may need `ufw allow 9742/tcp` or equivalent.

## Uninstall

Full removal:

```sh
# macOS
launchctl unload ~/Library/LaunchAgents/com.sovereign.daemon.plist
rm ~/Library/LaunchAgents/com.sovereign.daemon.plist
rm -rf ~/.svrnmesh

# Linux
systemctl --user disable --now sovereign
rm ~/.config/systemd/user/sovereign.service
systemctl --user daemon-reload
rm -rf ~/.svrnmesh
rm -rf ~/.config/sovereign
```

`sovereign setup --reset` is sufficient for a "start over" — it removes the service and config but keeps downloaded models (rerun downloads cheaply resume). Remove `~/.svrnmesh/models/` manually if you want to force a fresh model pick.
