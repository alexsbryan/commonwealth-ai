# corpus-mcp

A corpus-engine MCP host that needs nothing but an OpenAI-compatible endpoint.

Two verbs. It SERVES a corpus over MCP, and it BUILDS one from a recipe —
both against nothing but an OpenAI-compatible endpoint.

```sh
# serve (the default; no subcommand)
llama-server -m Qwen3-Embedding-0.6B-Q8_0.gguf --embeddings --port 8080
corpus-mcp --base-url http://localhost:8080/v1 --corpus sep

# build. llama-server loads one model per process, so chat and embeddings
# are two of them; Ollama serves both, and takes a single --base-url.
llama-server -m Qwen3-Embedding-0.6B-Q8_0.gguf --embeddings --port 8089 &
llama-server -m <an-instruct-model>.gguf --port 8090 &
corpus-mcp ingest my-coins.toml --chat-url  http://localhost:8090/v1                                 --embed-url http://localhost:8089/v1
```

`ingest` runs the whole of `sovereign/docs/specs/EPISTEMIC_INDEX.md` §4:
acquire → extract → chunk → embed → index, then the atlas enrichment (seed,
extract, cluster, name, resolve, tensions, gaps, configure, report, backfill),
ending in a v2 atlas store with a seed table and an `ontology.json` — the
thing `ask` walks. Neither half is implemented here: the recipe pipeline is
`corpus-engine`'s and the enrichment is `sovereign-enrichment-build`'s, both
of which now sit inside this package's boundary. What this binary adds is the
two-endpoint resolution and one `config.json`.

Two things a bare endpoint does not have, and the run says both out loud
rather than leaving them to be inferred from a slow phase: **GLiNER is not
linked** (it rides `ort`, which the boundary forbids), so the entity pass is
the chat model's; and **structured output is plain `response_format:
{type: "json_schema"}`**, which llama-server and Ollama both honour, refined
only if the host advertises otherwise at `/oicp/v1/capabilities`. A phase
whose schema the endpoint rejects fails by name; it is not skipped.

It speaks MCP over stdio and exposes five tools: `ask` (the composed
default — cited passages plus the map of ideas the atlas walk traversed),
`corpus_list`,
`corpus_search` (cited chunks from the same LanceDB + Tantivy hybrid every
sovereign surface uses), `atoms_lookup` (the atoms a corpus's enrichment
produced, read from `atlas/atoms.json`) and `corpus_ontology` (what the corpus
declared, from `atlas/ontology.json` — read through the writer's own
`read_atlas_ontology`, because the file is an `AtlasOntologyFile` envelope and
parsing it as bare policies silently yields an empty declaration). `--base-url`
is the only flag a serve requires;
whether the host is an OICP daemon or a bare `llama-server` is detected from
`GET /oicp/v1/capabilities`, and a 404 there is the normal case. Every
degradation — a width mismatch between an index and the endpoint's embeddings,
a corpus with no atlas — is printed to stderr and reported in the tool result,
never defaulted.

How the enrichment reaches a request here: it does not, unless the client
asks. `corpus_search` is tier 1 alone — nothing from the atlas or the
ontology touches its ranking or its results. The MCP client composes the
tiers itself, by calling `atoms_lookup` / `corpus_ontology` after (or instead
of) a search; the server's `instructions` string says so. For SEP
specifically: the corpus declared no ontology (`corpus_ontology` refuses by
path), its vocabulary is the fixed atom kinds its `philosophy_atlas` pipeline
extracts, and the searchable `sep` index carries an empty atlas — the atoms
live in the per-article `sep-<slug>` atlas dirs, which `atoms_lookup` reads by
name.

What it deliberately is not: the atom-grounded *ranking* that sovereign's chat
surfaces run (`atom_enum`, `atlas_grounding` in `sovereign-core`) is not here.
Tier 1 (cited chunk search) and tier 1.5 (reading what enrichment produced and
declared) cross the seam; the ranking is a separate extraction. Say that to
anyone who asks whether this is "the same retrieval".

The dep tree carries no llama.cpp, ort, iroh, mesh transport or agent runtime.
`tests/no_inference_stack.rs` asserts it against `cargo tree`;
`quality/ARCH_LAYERS.toml` declares the in-repo closure as a `[[package]]` that
boundary-gate enforces. `acceptance.sh` is the end-to-end proof: a real
`llama-server`, a real query over stdio, cited chunks and Claim atoms back.
Its slow leg (`ACCEPT_INGEST=1 CHAT_GGUF=…`) runs `ingest` on the committed
`sovereign-recipes/wessex-hoard` fixture against two bare processes and scores
the atlas it produces against that fixture's `truth.json`, beside the
daemon-built corpus of the same name — the control it must not fall below.
Without those variables the leg reports NEVER-RAN by name; it is not skipped.

Data root: the same derivation every sovereign binary uses
(`SOVEREIGN_DATA_DIR`, else `~/.svrnmesh`), or `--data-dir`. Serving reads
`<root>/indexes/<corpus>/` and writes nothing. `ingest` writes: the recipe to
`<root>/recipes/<id>/recipe.toml` (where the registry looks for it), the index
and chapter manifest to `<root>/indexes/<id>/`, and the enrichment config and
its run artefacts to `<root>/enrichment/<id>/`.
