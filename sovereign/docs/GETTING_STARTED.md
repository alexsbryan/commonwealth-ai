# Getting Started with Sovereign

**Sovereign turns documents you already have — notes, papers, a mailbox, a folder of PDFs — into a knowledge base you can ask questions of. It runs entirely on your machine and answers *grounded in your sources*, with citations, rather than from a model's memory.**

This guide starts from the questions you might be asking.

---

### "I just want to see what this does."

Open **Recipe Author** and click **Try a sample corpus**. It restores a small, ready-made corpus (a few of *The Federalist Papers*) in about a second — no setup, nothing downloaded — and drops you into a chat with a few suggested questions. Ask one; you'll watch it answer from the actual text. That round trip *is* the whole product in miniature.

### "I have a pile of documents and want to ask questions without the AI making things up."

Build a corpus from them:

1. **+ New project** — describe in a sentence or two what you're building (the *charter*) and where your documents live. Starting points are offered for a **folder of files**, a **CSV / spreadsheet**, a **mailbox**, a **website**, or a **web API**.
2. The assistant drafts the *recipe* — how to read, split, and organize your documents — in the chat beside you. When it's ready, click **Build & enrich**.
3. When it finishes, a **Use this corpus** panel appears with a few questions mined from your own documents — pick one, or click **Open in chat**. Either way you land in a grounded chat: answers cite the passages they came from, so you can check them, and it tells you when it *can't* find support instead of guessing.

### "My field has its own concepts and jargon a generic AI doesn't understand."

This is the reason the tool exists. As you build your corpus, the assistant interviews you about your domain — the entities, relationships, and questions that actually matter in *your* field (coins and mints; clauses and parties; symptoms and treatments — whatever yours is). It writes that into the recipe as your **ontology**, and enrichment extracts exactly those things. The result is a knowledge graph in *your* language, not an off-the-shelf one — and it's what the suggested questions and grounded answers are built from.

### "How do I know I can trust the answers?"

Two ways. Every answer is **grounded**: it retrieves real passages from your corpus and cites them, and abstains when it can't find support. And the **Authoring harness** (in the project dashboard) runs checks on a freshly built corpus — confirming it actually extracted what your ontology asked for — *before* you rely on it.

### "Will my data leave my machine?"

No. Reading your files, building the knowledge graph, and running the model all happen locally. Nothing is uploaded.

### "I want to start now, but my own files aren't ready."

Start with the **sample corpus** to learn the flow end to end, then come back and **+ New project** with your own. You don't have to get the recipe right by hand — describe what you want and let the assistant draft it.

---

## The tools, at a glance

| What you want | Where to go |
|---|---|
| Try it instantly | Recipe Author → **Try a sample corpus** |
| Build a corpus from your files | Recipe Author → **+ New project** (folder · CSV · mailbox · website · API) |
| Teach it your domain's concepts | Describe your field when the assistant asks — it writes your **ontology** into the recipe |
| Turn the draft into a working corpus | **Build & enrich** |
| Check it's sound before trusting it | **Authoring harness** (project dashboard) |
| Ask questions, grounded in your sources | **Use this corpus** → chat (cited answers + suggested questions) |
| Quickly index a folder, no project | Settings → **Knowledge** → drop a folder |

---

## For power users (command line)

Everything above maps to a CLI flow:

```
sovereign corpus install <id>     # build a corpus from a recipe
sovereign enrich init <id> --from-corpus <id>   # scaffold enrichment (auto-detects a custom ontology)
sovereign enrich build <id> --full              # run the atlas pipeline → atoms
sovereign chat                                  # grounded chat over installed corpora
```

Recipes are plain TOML you can hand-edit (the desktop's **Technical detail** drawer exposes the same file). A custom domain ontology is an `[enrichment.ontology]` block — see [`specs/CUSTOM_ATLAS.md`](specs/CUSTOM_ATLAS.md) for how it shapes extraction. For the system map, see [`../SYSTEM_OVERVIEW.md`](../SYSTEM_OVERVIEW.md).
