# corpus-mcp

A corpus-engine MCP host that needs nothing but an OpenAI-compatible endpoint.

## Three commands

You have a folder of documents, an MCP-capable chat app, and a machine that
runs Ollama or llama-server. That is the whole prerequisite list.

```sh
corpus-mcp recipe new --ontology numismatics --id my-coins   # writes my-coins.toml
corpus-mcp ingest my-coins.toml                              # acquire → chunk → embed → enrich → index
corpus-mcp serve --corpus my-coins                           # the MCP host, on stdio
```

Step 1 scaffolds a recipe from a built-in ontology template — `--ontology
list` names them all — and leaves you two things to fill in: `path`, the
folder your documents are in, and the type guidance. It never overwrites.

Step 2 builds the corpus. Step 3 serves it. **Neither line names a URL**, and
that is deliberate: with no endpoint given, both walk the same ladder and
print what each rung said.

    --base-url <url>              if you passed one — and if it does not
                                  answer, that is a refusal, never a
                                  fall-through to something else
    http://localhost:11434/v1     Ollama
    http://localhost:8080/v1      llama-server
    the OICP daemon               this host's own, if you happen to run one

`corpus_list` reports the same ladder to the chat app, so a stopped Ollama is
visible where you are actually looking.

Name the endpoint yourself when you want to:

```sh
# One host serving both chat and embeddings (Ollama, vLLM, our own daemon).
corpus-mcp ingest my-coins.toml --base-url http://localhost:11434/v1

# llama-server loads ONE model per process, so chat and embeddings are two of
# them and discovery cannot tell which port is which. Name them.
llama-server -m Qwen3-Embedding-0.6B-Q8_0.gguf --embeddings --port 8089 &
llama-server -m <an-instruct-model>.gguf --port 8090 &
corpus-mcp ingest my-coins.toml --chat-url  http://localhost:8090/v1 \
                                --embed-url http://localhost:8089/v1
```

### Ollama

Ollama is the shape these commands are written for — one process, one URL,
both models — and it is the first rung of the ladder for that reason. It is
NOT the tested default: the acceptance suite runs against `llama-server`,
which is what the development host has. The Ollama path is exercised by the
same code on the same flags and has not been run end to end against a live
Ollama; if you find a difference, it is a bug and not a design.

### A named corpus is pulled if you do not have it

```sh
corpus-mcp serve --corpus sep     # ~875 MB from HuggingFace on a cold machine
```

`serve` installs a corpus you named but do not have, when its recipe declares
a prebuilt snapshot: the archive carries the chunk index AND the atlas, so
what you get is the enriched corpus, not a re-embed. A corpus whose recipe
declares no snapshot is **not** pulled — building it is `corpus-mcp ingest`'s
job, and serving will not quietly start an hours-long acquire on your behalf.
Either way you are told which.

## The MCP config block

One block, in your chat app's MCP settings:

```json
{
  "mcpServers": {
    "corpus": {
      "command": "corpus-mcp",
      "args": ["serve", "--corpus", "my-coins"]
    }
  }
}
```

Add `"--base-url", "http://localhost:11434/v1"` to `args` to skip discovery.
Everything diagnostic goes to stderr — stdout is the MCP channel — so the
probe findings above appear in your app's server log, and in `corpus_list`.

## What the verbs do

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
parsing it as bare policies silently yields an empty declaration). No flag is
required to serve; whether the host is an OICP daemon or a bare `llama-server`
is detected from
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

**Budget for the thinking, not just the answer.** A thinking model whose
chain of thought does not close inside the output budget returns an EMPTY
`content` from an OpenAI-compatible endpoint, not an error: llama-server puts
the reasoning in `reasoning_content` and fills `content` only once thinking
terminates. Phase 1 then reports `<empty response>`. The same endpoint, model
and schema, two requests apart:

    max_tokens 64    -> content ""                       (64 tokens spent)
    max_tokens 16384 -> content {"capital": "Paris"}     (166 tokens, stop)

So the endpoint honours `response_format: json_schema` — the JSON came back
conforming — and the failure is the budget, not the schema.

**This is the bare-endpoint path, not one small model.** Measured 2026-09-05
on the wessex fixture, 20 chapters: `Qwen3.6-35B-A3B` needed the terse retry
on **18 of 20 chapters**, and recovered all 18 — phase 1 spent 81,403
completion tokens on the first pass and 120,193 more on the retry, against
17,940 for the same extraction through the daemon. The 4B differs in degree,
not in kind: it also recovered on retry, but at a rate that put a 20-chapter
run into hours. Budget for both passes and expect roughly 11x the daemon's
completion tokens.

Two phases return nothing at all here, on both runs and both models. Phase 3
(cluster naming) reports `0/N named, N failed (ParseDrift)`; phase 6 (the
tension classifier) reports `0 chat + N parse failure(s) — recall degraded`,
N being every candidate. Extraction and resolve are unaffected — they produced
172 and 165 atoms against the daemon control's 159 — so what a bare endpoint
loses is the classified layer above the atoms, not the atoms. Both phases say
so in the run rather than passing quietly.

Data root: the same derivation every sovereign binary uses
(`SOVEREIGN_DATA_DIR`, else `~/.svrnmesh`), or `--data-dir`. Serving reads
`<root>/indexes/<corpus>/` and writes nothing. `ingest` writes: the recipe to
`<root>/recipes/<id>/recipe.toml` (where the registry looks for it), the index
and chapter manifest to `<root>/indexes/<id>/`, and the enrichment config and
its run artefacts to `<root>/enrichment/<id>/`.
