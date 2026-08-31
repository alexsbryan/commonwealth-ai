# Use your own inference server

Sovereign runs models itself, using llama.cpp built into the daemon. If you
already have a serving stack you'd rather use — vLLM, SGLang, TGI, or something
you tuned yourself — you can point Sovereign at it instead. Everything above the
model stays as it is: your corpora, retrieval, citations, the mesh, the editor
integration. Only the thing generating tokens changes.

This is a config change, not a build. One section in `~/.svrnmesh/config.toml`.

## When you'd want this

The built-in engine is a good default and it needs nothing from you. Reach for
your own server when you have a reason:

- **Your hardware wants a different runtime.** You've tuned vLLM for your GPUs,
  or you're on something llama.cpp doesn't target well.
- **You need throughput Sovereign's engine doesn't reach.** Continuous batching
  across many concurrent users is what vLLM and SGLang are built for.
- **The model isn't in GGUF.** A brand-new architecture usually lands in
  transformers and vLLM well before llama.cpp, and waiting for the conversion is
  the slow path.
- **You already run one.** There's no reason for two copies of the same weights
  resident on one box.

The trade: Sovereign no longer knows what's loaded or how much memory it's
using, so it stops managing that for you. It can't swap models between requests,
it can't unload an idle model to reclaim memory, and it can't tell you what's
resident — because on the other side of an HTTP call, it genuinely doesn't know.
Your server owns those decisions now.

## Point it at one server

Start your server however you normally do. vLLM, for example:

```sh
vllm serve Qwen/Qwen3.5-35B-A3B --port 8000
```

Then put this in `~/.svrnmesh/config.toml`:

```toml
[engine]
kind = "remote"
endpoint = "http://localhost:8000/v1"
model_id = "Qwen/Qwen3.5-35B-A3B"
context_size = 32768
```

`model_id` goes on the wire as the `model` field, so it has to be a name your
server actually answers to — it's routing, not a label. `context_size` is what
Sovereign budgets prompts against; set it to the window your server was started
with, because it has no way to ask.

Restart the daemon and check it took:

```sh
svrn daemon stop && svrn daemon start
svrn daemon status
```

You should see the daemon come up without loading anything. Ask it something:

```sh
curl -s localhost:9741/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"Qwen/Qwen3.5-35B-A3B","messages":[{"role":"user","content":"hello"}]}'
```

If your server needs a key, add `api_key = "..."` — it's sent as an
`Authorization: Bearer` header.

## Add an embedding model, or lose retrieval

This is the step people miss, so it's worth being blunt about.

vLLM, SGLang and TGI serve **one model per process**. Your chat server can't
embed. But Sovereign's corpora, memory and citations all need embeddings, so
unless you tell it otherwise it will send embedding requests to your chat model
and get back something that isn't an embedding — and retrieval quietly degrades
rather than failing loudly.

So run a second server for the embedding model:

```sh
vllm serve BAAI/bge-m3 --port 8001
```

and name it:

```toml
[engine]
kind = "remote"
endpoint = "http://localhost:8000/v1"
model_id = "Qwen/Qwen3.5-35B-A3B"
context_size = 32768

embed_endpoint = "http://localhost:8001/v1"
embed_model_id = "BAAI/bge-m3"
```

Chat and embeddings now go to their own servers with their own model names. If
one server happens to serve both, set `embed_model_id` alone and leave
`embed_endpoint` out — it defaults to `endpoint`.

Two things to know about embeddings specifically. **Changing the embedding model
invalidates the corpora you built with the old one** — vectors from two different
models aren't comparable, and Sovereign records which model produced each one so
it can refuse to mix them. If you switch, re-ingest. And **instruction-aware
embedding models lose a little quality here**: models like Qwen3-Embedding want a
different prefix on the query side than the document side, and Sovereign doesn't
know which prefix your model expects. It sends neither rather than inventing one.
That's worth 1–5% on retrieval; a model without that asymmetry loses nothing.

You can skip all of this if you don't use corpora, memory, or anything that
retrieves — plain chat needs no embedding model.

## What still works, and what doesn't

Chat, streaming, tool calls, the OpenAI-compatible API on `:9741`, corpora and
citations, the mesh, editor completion — all unchanged. Sovereign is doing the
same work; it's just asking a different process for tokens.

Some things change, and they're the ones tied to Sovereign holding the weights
itself.

**Structured output depends on your server now.** When Sovereign wants JSON
matching a schema it sends it as the standard `response_format` field, so a
server with guided decoding — vLLM and SGLang both have it — will enforce it as
before. A server that ignores the field returns ordinary text, and you'll find
out at the point something fails to parse rather than up front. Sovereign's own
grammar constraints are a private extension and no third-party server implements
them, so those become suggestions.

**Reranking is off** unless you run a reranker yourself; Sovereign's is part of
the built-in engine. Retrieval falls back to un-reranked results, which is a
quality step down, not a failure.

**`svrn status` reports no models resident**, because none are. Ask your own
server what it's holding.

Sovereign won't paper over any of these. A feature that needs local weights
reports itself unavailable rather than pretending it worked.

## If your engine doesn't speak the OpenAI API

`kind = "remote"` talks the OpenAI API, which is what vLLM, SGLang, TGI,
llama-server, LM Studio, Ollama and most hosted providers speak. If your engine
speaks that, you're done — the name on the box doesn't matter.

If it doesn't, or you want it in the daemon's own process rather than behind
HTTP, you can compile an engine in. That means implementing four methods in Rust
and registering it at startup, in a build of the daemon you control — Rust can't
load one from a shared library safely. The walkthrough is a runnable file:

```sh
cargo run -p sovereign-inference --example custom_engine
```

[`sovereign/crates/sovereign-inference/examples/custom_engine.rs`](../sovereign/crates/sovereign-inference/examples/custom_engine.rs)
is the template — a working engine, the config that selects it, and the
conformance check you run before trusting it. That check is worth using: it
catches the mistakes that otherwise surface much later as a truncated answer or
a silently mis-ranked corpus.

## Going back

Delete the `[engine]` section, or set `kind = "llama"`, and restart. Your
`[models]` paths were never touched while the remote engine was running, so the
built-in engine picks up exactly where it left off.

## Troubleshooting

**The daemon refuses to start, naming a model file.** Something still expects a
local model. `[models]` is required by the config schema even with a remote
engine — the paths just need to be *present*, not real. If the daemon is
checking whether they fit in memory, it thinks it's running the built-in engine;
confirm `kind = "remote"` is spelled correctly and that you edited the config the
daemon actually reads (it prints the path when started with `--config`).

**Answers arrive but retrieval finds nothing relevant.** Embeddings are going to
your chat model. See the embedding section above.

**`svrn status` shows no models.** That's correct and not a fault — this node
holds none.

**Everything is slower than expected.** You've added a network hop per request.
On localhost it's noise; across machines it isn't, and a busy server's queue
shows up here as latency Sovereign can't see into.
