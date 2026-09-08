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
both models — and it is the first rung of the ladder for that reason. It was
run against a live Ollama for the first time on 2026-09-07 (v0.33.3, installed
under `~/.local/ollama` with no root; artefacts in
`runs/ei3c-ollama-arm/evidence-2026-09-07-run3/`). What that measured:

- **Discovery works.** The ladder named Ollama at rung 1 and stopped there —
  `endpoint candidate ollama http://localhost:11434/v1 — 2 model(s) listed`.
- **The width matches.** `qwen3-embedding:0.6b` returns **1024-d**, the width
  the shipped indexes were built at, so `corpus_list` reports `sep — 1024-d,
  vector + full-text` and nothing degrades to full-text.
- **You must name the embedding model.** With no `--embed-model`, corpus-mcp
  sends the first id `GET /v1/models` returns. Ollama serves chat AND
  embeddings from one URL and lists them in its own order, so that first id was
  `qwen3:0.6b` — the chat model — and `POST /v1/embeddings` came back **501 Not
  Implemented**. Pass it explicitly:

```sh
ollama pull qwen3-embedding:0.6b
corpus-mcp serve --corpus sep --base-url http://localhost:11434/v1 \
                 --embed-model qwen3-embedding:0.6b
```

  That invocation served `sep` over stdio and answered `corpus_list`. This is
  the one place the single-URL shape costs you a flag, and it is a rough edge
  in the default, not in Ollama.

Installing it without root, which is how the run above was done:

```sh
curl -LO https://github.com/ollama/ollama/releases/download/v0.33.3/ollama-linux-amd64.tar.zst
curl -LO https://github.com/ollama/ollama/releases/download/v0.33.3/sha256sum.txt
sha256sum -c <(grep ollama-linux-amd64.tar.zst sha256sum.txt)
mkdir -p ~/.local/ollama && tar --zstd -C ~/.local/ollama -xf ollama-linux-amd64.tar.zst
export PATH="$HOME/.local/ollama/bin:$PATH" OLLAMA_MODELS="$HOME/.local/share/ollama"
ollama serve &
ollama pull qwen3-embedding:0.6b && ollama pull qwen3:0.6b
```

The tarball is 1.4 GB and unpacks to 2.2 GB. **On an integrated GPU it runs on
the CPU unless you say otherwise**: this run logged `dropping integrated GPU; to
enable, set OLLAMA_IGPU_ENABLE=1` for `AMD Radeon 8060S Graphics (RADV
GFX1151)` and fell back to `inference compute id=cpu`. On a machine whose only
GPU is integrated — a Strix Halo, say — `OLLAMA_IGPU_ENABLE=1` **is** the GPU
path. The measurement above was taken on the CPU, which is fine for it: it is
about discovery, model choice and embedding width, none of which is a function
of the device.

### A named corpus is pulled if you do not have it — but read this first

```sh
corpus-mcp serve --corpus sep     # ~875 MB from HuggingFace on a cold machine
```

`serve` installs a corpus you named but do not have, when its recipe declares
a prebuilt snapshot: the archive carries the chunk index AND the atlas, so
what you get is the enriched corpus, not a re-embed. A corpus whose recipe
declares no snapshot is **not** pulled — building it is `corpus-mcp ingest`'s
job. Either way you are told which.

**A shipped snapshot will very likely be REJECTED, and this is measured, not
hypothetical.** Before it trusts a snapshot, the restore re-embeds a sample of
that snapshot's own chunks through your endpoint and compares them to the
stored vectors, requiring a mean cosine of at least 0.92. On 2026-09-05,
`sep`'s snapshot against a bare `llama-server --embeddings` on the *identical*
`Qwen3-Embedding-0.6B-Q8_0.gguf` scored **0.68**.

Until 2026-09-07 this section blamed the EOS token and told you a bare
endpoint could not match ours. **Both halves of that were wrong**, and the
measurement that corrected them is in `test-artifacts/ei6b-mechanism/`:

- A bare `llama-server` reproduces *our daemon's* embeddings at **0.9956**
  cosine on raw text, and **0.9998** once the caller prepares inputs the way
  this crate now does. There was never a meaningful endpoint gap.
- Our own daemon scores **0.698** against `sep`'s stored vectors — it cannot
  reproduce them either. Whatever is wrong is not in the caller.
- Re-embedding `sep`'s chunks with `--pooling mean` scores **0.9997**.

`sep` and `wikipedia` were built **mean-pooled**. Qwen3-Embedding is a
**last-token-pooled** model and the current stack pools last, which every
corpus built since July 2026 confirms:

| corpus | built | `--pooling last` | `--pooling mean` |
|---|---|---|---|
| `sep` chunks | 2026-04-09 | 0.7069 | **0.9997** |
| `wikipedia` chunks | 2026-04-29 | 0.7192 | **0.9573** |
| `wessex-hoard` chunks | 2026-09-02 | **0.9968** | 0.6615 |
| `sep-al-farabi` atlas seeds | 2026-09-05 | **0.9610** | 0.5131 |

So the probe is right to refuse those two snapshots, no endpoint can fix it,
and `SOVEREIGN_FORCE_PREBUILT=1` on them would install vectors your queries
cannot reach. They need re-publishing from the current stack; that is tracked,
and it is not something a user can do locally.

For every other corpus — anything built by the current stack — a bare
llama-server is a first-class endpoint. Which is what this crate is for.

### What this crate does about it

`corpus-mcp` now prepares embed inputs the way the corpus was built, from one
table (`sovereign_contracts::embed_quirks`) shared with the daemon rather than
re-derived here:

- The embed family is resolved from the endpoint's own model id. **An
  unrecognised id gets no quirks at all** — raw text, said out loud at boot and
  reported in `corpus_list` — never some other family's instruction prefix.
- Documents get the document-side preparation; questions and atlas seed tables
  get the **query-side** one. These are different vector spaces on an
  instruction-aware embedder, and mixing them costs real recall:

| what a question is embedded with | cosine vs. the atlas seed table |
|---|---|
| raw text | 0.8605 |
| + query instruction | 0.9841 |
| + query instruction + EOS | **0.9888** |

  Measured on `wessex-hoard`, a corpus the current stack built. Until
  2026-09-07 this crate sent raw text on both sides, so its own seed tables
  were written in one space and searched in another — the two errors cancelled
  and matched no daemon-built atlas.
- Snapshots published from now on record the embedder configuration they were
  built under, so a mismatched space is refused **from the manifest**, naming
  pooling, before a byte is downloaded. Snapshots published before then carry
  no such field, and the refusal says so rather than printing a bare cosine.

### Every restore is judged now, including a local one

`svrn corpus snapshot restore --archive <path>` used to extract whatever it was
handed — no probe, no verdict. A snapshot in the wrong embedding space
installed silently, on the one path with no deadline to stop it. Both restore
paths now run the same decision:

- If the manifest **declares** an embedder config and it differs from yours,
  the restore is refused off the manifest, naming the difference, before a byte
  is extracted.
- If it declares none (every snapshot published before 2026-09-07), a sample of
  the archive's own chunks is re-embedded through your endpoint and compared to
  the stored vectors — the 0.92 bar above.
- If neither can be answered — no embedder reachable, say — the verdict is
  `COULD-NOT-JUDGE` **and the extracted index is removed**. An unjudged archive
  is not installed.

You can see the whole loop locally, with no network:

```bash
svrn corpus snapshot publish my-corpus --output /tmp/my-corpus.tar.zst
svrn corpus snapshot restore --archive /tmp/my-corpus.tar.zst --into /tmp/coldroot \
     --embedding-model <your embed model> --embedding-dim 1024
# → ✓ Restored — accepted (embedding-space probe, probe cosine 0.9999)
```

So on a bare endpoint against one of the two stale snapshots, expect this:

    corpus-mcp serve --corpus sep
    → snapshot refused (probe_cosine=0.68 < 0.92); the snapshot declares no
      embedder config, so pooling could not be compared before downloading —
      a different POOLING is the usual cause, and no re-embedding on this host
      can fix it: the corpus must be re-published from the current stack
    → full rebuild starts, then is refused, because a rebuild of sep is hours

That refusal is deliberate — `serve` will not silently spend hours rebuilding
a corpus you asked it to fetch. Your options:

1. **Build the corpus yourself** — `corpus-mcp ingest <recipe.toml>`. The
   vectors are then yours and match your endpoint by construction. This is the
   honest path for the two stale snapshots, and it is what the three-command
   experience above is for.
2. **Use an endpoint matching the one that built it.** For `sep` and
   `wikipedia` today that means a mean-pooled embedder, which is not what our
   own daemon runs either.
3. `--pull-deadline-mins 0` disables the bound if you genuinely want to wait
   out a rebuild.
4. `SOVEREIGN_FORCE_PREBUILT=1` skips the probe. **Do not use it on `sep` or
   `wikipedia`** — the mismatch there is real and you would get an index whose
   vectors disagree with your own queries, degrading retrieval quietly instead
   of failing.

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

**Thinking is switched off per request, in the one spelling a bare endpoint
reads.** A thinking model whose chain of thought does not close inside the
output budget returns an EMPTY `content` from an OpenAI-compatible endpoint,
not an error: llama-server puts the reasoning in `reasoning_content` and
fills `content` only once thinking terminates, and a JSON grammar does not
stop it thinking first. Until 2026-09-08 the enrichment client asked for no
thinking in two spellings the daemon and DeepSeek read (`think_budget: 0`,
`thinking: {type: disabled}`) and not the one llama-server, vLLM and SGLang
read — `chat_template_kwargs: {enable_thinking: false}` — so on a bare
llama-server every phase thought through its budget under the grammar. That
was the whole of the "budget for the thinking" story this section used to
tell: measured 2026-09-05 on the wessex fixture, `Qwen3.6-35B-A3B` needed the
terse retry on 18 of 20 chapters and spent 201,596 completion tokens against
the daemon's 17,940, and phases 3 and 6 returned empty `content` for every
candidate. With the kwarg sent, the same two attribution chapters extract on
the first attempt in 48 s for both, and the scholars come back as `person`
entities rather than coins. The `/no_think` soft switch and a
`reasoning_budget` field do nothing here; only the template kwarg does, and
only on a template that declares `enable_thinking` — a thinking-only build
(the plain `Qwen3.5-4B.Q6_K` template has no such variable) cannot be
switched per request, and that is a fact about the model to name, not a
budget to raise.

**What a bare llama-server still loses, isolated and open.** The same
`corpus ingest`, prompt, schema and 35B, chapters `sec_00014` + `sec_00012`
of the fixture: through the daemon (`--base-url http://127.0.0.1:9741/v1`)
Phase 1 returns 6 claims, 4 of them `attributed_to` Halstead or Ferreira;
through llama-server (`--chat-url`) it returns 0 claims, thinking on or off —
the sketch carries no `claims` key at all. Entities, relations and questions
are unaffected. The difference is in how the endpoint turns the JSON schema
into a grammar (llama.cpp's converter against the daemon's llguidance; the
`claims` property is optional at the top level), and it is the remaining gap
between this path and the control on `truth.json`'s attribution rows. It is
reported by name in the run's `[done] resolve` line (`0 claim atom(s)`),
never inferred.

Data root: the same derivation every sovereign binary uses
(`SOVEREIGN_DATA_DIR`, else `~/.svrnmesh`), or `--data-dir`. Serving reads
`<root>/indexes/<corpus>/` and writes nothing. `ingest` writes: the recipe to
`<root>/recipes/<id>/recipe.toml` (where the registry looks for it), the index
and chapter manifest to `<root>/indexes/<id>/`, and the enrichment config and
its run artefacts to `<root>/enrichment/<id>/`.
