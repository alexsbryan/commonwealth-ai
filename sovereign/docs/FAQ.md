# Common questions

Short answers to what people ask first. If something's broken, [TROUBLESHOOTING](TROUBLESHOOTING.md) goes deeper and `sovereign doctor` checks most of it for you.

## Do I need an internet connection?

No. Once `sovereign setup` has downloaded your models and any knowledge bases, everything runs offline. Web search is the one exception, and it's optional — set a Brave or Tavily API key if you want it, skip it otherwise.

## Can I use a remote model instead of the local one?

You can, though it isn't wired into the CLI yet. For now the simplest way is to point your client — opencode, say — straight at the remote provider, and leave Sovereign's daemon running for the local tools and knowledge search. First-class remote-model setup is on the list.

## How do I switch to a different primary model?

Edit `~/.sovereign/config.toml`:

```toml
[models]
primary = "/path/to/new-model.gguf"
```

Then restart the daemon — the [troubleshooting guide](TROUBLESHOOTING.md#want-to-switch-models-after-setup) has the per-platform command — or run `sovereign setup --reset` to re-download with the picker.

## Why ports 9741 and 9742?

`:9741` serves both the OpenAI-compatible `/v1` API and the MCP `/mcp` endpoint. `:9742` carries the internal traffic between mesh nodes and isn't exposed to your own code. Both can be changed in the config file if something else wants the port.

## Can I run more than one instance on a machine?

Not by default — the daemon is a single per-user service. You can run a second by giving it different ports (`client_port` / `internal_port` in config.toml) and a separate `--data-dir`, but most people want just the one.

## What's the difference between Commonwealth and Sovereign?

Sovereign is the assistant you use: chat, knowledge bases, code intelligence, skills. Commonwealth is the mesh layer underneath it, where a few people share inference across their machines. It runs inside Sovereign — you never install or start it separately. From where you sit there's one command (`sovereign`), one daemon, one port; the mesh is just `sovereign mesh create` / `join` / `rotate`.

## Where do my models, corpora, and indexes live?

- Models — `~/.sovereign/models/*.gguf`
- Config — `~/.sovereign/config.toml`
- Logs — `~/.sovereign/logs/daemon.log`
- Code and knowledge indexes — `~/.sovereign/indexes/<corpus>/` (downloaded shards land in `_downloads/` and index into the same place)
- Mesh state — `mesh.json`, in your data directory (`~/.sovereign/` by default, on both macOS and Linux). Deleting it resets the mesh; `sovereign mesh leave` is the clean way to do that.

## How do I set a web search API key?

For now you pass it when you start the REPL — `sovereign --model <path> --brave-api-key <key>`, or `--tavily-api-key`. Storing it through setup is on the list; until then, keep it in your shell environment and expand it yourself.

## What's the difference between `project init` and `code index`?

`sovereign project init` is the whole setup: the symbol index, the call graph, the `.claude` / `.opencode` wiring, and the git hooks. `sovereign code index <path>` is just the first piece, the symbol pass on its own. Use `project init` for the full thing; reach for `code index` when you're scripting something narrow.
