# Common questions

Short answers to what people ask first. If something's broken, [TROUBLESHOOTING](TROUBLESHOOTING.md) goes deeper and `sovereign doctor` checks most of it for you.

## Do I need an internet connection?

No. Once `sovereign setup` has downloaded your models and any knowledge bases, everything runs offline. Web search is the one exception, and it's optional — set a Brave or Tavily API key if you want it, skip it otherwise.

## Can I use a remote model instead of the local one?

You can, though it isn't wired into the CLI yet. For now the simplest way is to point your client — opencode, say — straight at the remote provider, and leave Sovereign's daemon running for the local tools and knowledge search. First-class remote-model setup is on the list.

## How do I switch to a different primary model?

Edit `~/.svrnmesh/config.toml`:

```toml
[models]
primary = "/path/to/new-model.gguf"
```

Then restart the daemon — the [troubleshooting guide](TROUBLESHOOTING.md#want-to-switch-models-after-setup) has the per-platform command — or run `sovereign setup --reset` to re-download with the picker.

## Why ports 9741 and 9742?

`:9741` serves both the OpenAI-compatible `/v1` API and the MCP `/mcp` endpoint. `:9742` carries the internal traffic between mesh nodes and isn't exposed to your own code. Both can be changed in the config file if something else wants the port.

## Can I run more than one instance on a machine?

Not by default — the daemon is a single per-user service. You can run a second by giving it its own ports and data dir — [two daemons on one machine](../../docs/JOIN_A_MESH.md#appendix-two-daemons-on-one-machine) has the working config — but most people want just the one.

## What's the difference between Commonwealth and Sovereign?

Sovereign is the assistant you use: chat, knowledge bases, code intelligence, skills. Commonwealth is the mesh layer underneath it, where a few people share inference across their machines. It runs inside Sovereign — you never install or start it separately. From where you sit there's one command (`sovereign`), one daemon, one port; the mesh is just `sovereign mesh create` / `join` / `rotate`.

## Where do my models, corpora, and indexes live?

- Models — `~/.svrnmesh/models/*.gguf`
- Config — `~/.svrnmesh/config.toml`
- Logs — `~/.svrnmesh/logs/daemon.log`
- Code and knowledge indexes — `~/.svrnmesh/indexes/<corpus>/` (downloaded shards land in `_downloads/` and index into the same place)
- Mesh state — `mesh.json`, in your data directory (`~/.svrnmesh/`; `~/.svrnmesh` is the legacy name for the same place). Deleting it resets the mesh; `sovereign mesh leave` is the clean way to do that.

## How do I set a web search API key?

Through the environment: `SVRNMESH_TAVILY_API_KEY` (the legacy `SOVEREIGN_TAVILY_API_KEY` spelling is still bridged at startup). There were `--brave-api-key` / `--tavily-api-key` flags on the old interactive REPL; that REPL became `svrn chat` in the 2026-05-22 split and the flags stopped being read then, so they were removed on 2026-08-23. Storing the key through setup is still on the list.

## What's the difference between `project init` and `code index`?

`sovereign project init` is the whole setup: the symbol index, the call graph, the `.claude` / `.opencode` wiring, and the git hooks. `sovereign code index <path>` is just the first piece, the symbol pass on its own. Use `project init` for the full thing; reach for `code index` when you're scripting something narrow.
