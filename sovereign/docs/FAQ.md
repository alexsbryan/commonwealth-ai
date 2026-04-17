# FAQ

← [back to README](../README.md)

### Do I need an internet connection to use Sovereign?

No. After `sovereign setup` downloads your models and any knowledge bases, everything runs offline. Web search is optional — set `--brave-api-key` or `--tavily-api-key` if you want supplemental web results, skip them otherwise.

### Can I use a remote model instead of the local one?

Yes. `sovereign-inference` supports an OpenAI-compatible `RemoteApiProvider`. For now this is configured at the library level; CLI-first remote-model setup is on the roadmap. Short-term, point opencode directly at the remote provider and leave Sovereign's local daemon serving MCP tools only.

### How do I switch to a different primary model?

Edit `~/.config/sovereign/config.toml` (Linux) or `~/Library/Application Support/sovereign/config.toml` (macOS):

```toml
[models]
primary = "/path/to/new-model.gguf"
```

Then restart the daemon (see [TROUBLESHOOTING.md](TROUBLESHOOTING.md#want-to-switch-models-after-setup)). Or run `sovereign setup --reset` for a full re-download with the picker.

### Why ports 9741 / 9742?

`:9741` serves both the OpenAI-compatible `/v1` API and the MCP `/mcp` JSON-RPC endpoint. `:9742` carries internal mesh gossip (never exposed to user code). Both are overridable in the config file if they conflict with another tool.

### Can I run multiple instances on one machine?

Not by default — the daemon is a user-level service. You can run a second instance by overriding ports (`client_port` / `internal_port` in config.toml) and pointing `--data-dir` at a separate path. This is rare; most users want one.

### What's the difference between Commonwealth and Sovereign?

- **Sovereign** is the per-user assistant: chat, knowledge bases, code intelligence, skills.
- **Commonwealth** is the mesh layer: multiple Sovereign users share inference compute with each other. It runs *inside* Sovereign as `EmbeddedDaemon` — you never install or run it separately.

From the user's point of view there's one binary (`sovereign`), one daemon (`sovereign daemon run`), one port (`:9741`). Mesh operations are `sovereign mesh create/join/rotate`.

### Where do my models / corpora / indexes live?

- **Models**: `~/.sovereign/models/*.gguf`
- **Mesh state**: platform-native data dir (e.g. `~/Library/Application Support/sovereign/mesh.json` on macOS)
- **Code intelligence indexes**: `~/.sovereign/indexes/<corpus>/`
- **Knowledge corpora**: `~/.sovereign/indexes/_downloads/<corpus>/` (shards) + indexed into the same corpus dir
- **Config**: `~/.config/sovereign/config.toml` (XDG) or `~/Library/Application Support/sovereign/config.toml` (macOS)
- **Logs**: `~/.sovereign/logs/daemon.log`

### How do I configure a web search API key?

At REPL invocation: `sovereign --model <path> --brave-api-key <key>`. For persistent storage via setup, roadmap; for now, add to your shell's environment and use `${BRAVE_API_KEY}`-style expansion in your own wrapper.

### What's the difference between `project init` and `code index`?

- `sovereign project init` is the full workflow: tree-sitter symbol index **and** SCIP call graph **and** `.claude`/`.opencode` wiring **and** git hooks.
- `sovereign code index <path>` is the low-level primitive: just the tree-sitter pass.

If you want call graphs and AI harness auto-detection, use `project init`. If you're scripting a narrow pipeline, `code index` is smaller.

### Where does mesh state persist?

`mesh.json` lives under `dirs::data_dir()` (macOS: `~/Library/Application Support/sovereign/`, Linux: `~/.local/share/sovereign/`). Deleting it resets the mesh; `sovereign mesh leave` is the supported way to do it cleanly.
