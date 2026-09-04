# corpus-mcp

A corpus-engine MCP host that needs nothing but an OpenAI-compatible endpoint.

```sh
llama-server -m Qwen3-Embedding-0.6B-Q8_0.gguf --embeddings --port 8080
corpus-mcp --base-url http://localhost:8080/v1 --corpus sep
```

It speaks MCP over stdio and exposes four tools: `corpus_list`,
`corpus_search` (cited chunks from the same LanceDB + Tantivy hybrid every
sovereign surface uses), `atoms_lookup` (the atoms a corpus's enrichment
produced, read from `atlas/atoms.json`) and `corpus_ontology` (what the corpus
declared, from `atlas/ontology.json` — read through the writer's own
`read_atlas_ontology`, because the file is an `AtlasOntologyFile` envelope and
parsing it as bare policies silently yields an empty declaration). `--base-url` is the only required flag;
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

Data root: the same derivation every sovereign binary uses
(`SOVEREIGN_DATA_DIR`, else `~/.svrnmesh`), or `--data-dir`. Indexes are read
from `<root>/indexes/<corpus>/`; nothing is written.
