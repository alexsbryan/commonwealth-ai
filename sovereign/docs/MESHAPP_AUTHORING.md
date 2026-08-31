# Build your own mesh app — recipe → corpus → explorer

You're inspired by the Enron explorer and want to make one over your own data. A mesh
app is two things: a **corpus** (your data, enriched into an atlas) and a **bundle**
(a small sandboxed web app that reads it through `window.meshApp`). This walks both.

The whole loop:

```
author a recipe → ingest + enrich → (publish a snapshot) → meshapp new → meshapp dev → ship
```

---

## Part A — get your data into an atlas corpus

A corpus is defined by a **recipe** (a TOML file): where the data comes from, how to
chunk it, and — the part that makes an explorer possible — an `[enrichment]` pass that
extracts the typed atom graph (entities, relations, events, claims) and reconciles
identities.

1. **Author the recipe.** Start from an existing one (e.g.
   `sovereign-recipes/enron-sample-multi-wide/recipe.toml`) and adapt it. The key
   sections:

   ```toml
   [corpus]
   id = "my-corpus"
   name = "My Corpus"
   size_indexed_gb = 0.4        # used by the app card's "Get data (… GB)"

   [acquire]                    # where the raw data is (local_file, http_archive, …)
   type = "local_file"
   path = "~/.svrnmesh/corpora-staging/my-corpus"

   [extract]                    # email | plaintext | html | …
   type = "email"

   [chunk]
   type = "paragraph"
   max_chars = 2000

   [index]
   fts = true
   vector = true

   [enrichment]                 # THIS is what makes an explorer possible
   enabled = true
   type = "atlas"               # entities/relations/events/claims + reconciliation
   domain = "business_email"
   ```

   Validate + dry-run before a full ingest:

   ```bash
   sovereign recipe validate ./recipe.toml
   sovereign recipe test ./recipe.toml          # sample ingest, no embedding
   ```

2. **Ingest + enrich.** Register the recipe locally, then install:

   ```bash
   sovereign recipe publish ./recipe.toml       # → ~/.svrnmesh/recipes/
   sovereign corpus install my-corpus           # ingest → embed → index → enrich (atlas)
   ```

   This produces `~/.svrnmesh/indexes/my-corpus/` with `atlas/` (atoms,
   reconciliation) + `chapters.json` + `chunks.lance`. That's everything the explorer
   ops read. Check it:

   ```bash
   sovereign corpus status | grep my-corpus
   ```

3. **(To share it) publish a prebuilt snapshot** so others get one-click data:

   ```bash
   sovereign corpus snapshot publish my-corpus  # → a tar.zst + manifest (push to HF)
   ```

   Note the resulting `hf_repo`, `hf_filename`, and `sha256` — they go in your
   recipe's `[prebuilt]` block, which is what the desktop's "Get data" uses.

---

## Part B — build the app on it

The bundle is composed from the **MeshApp SDK** (`public/meshapp/_sdk/`,
dependency-free ES modules). You almost never write DOM — you compose components.

4. **Scaffold** an SDK-composed explorer:

   ```bash
   sovereign meshapp new my-explorer --corpus my-corpus
   ```

   This writes `public/meshapp/my-explorer/{index.html, app.js, meshapp.json}` — a
   working explorer: a scale banner, a force-directed graph with a type toggle, search,
   and the cited drill-down.

5. **Iterate against real data** — no desktop rebuild:

   ```bash
   sovereign meshapp dev my-explorer            # serves at http://127.0.0.1:4317/
   ```

   Open the URL, edit `app.js` / `index.html`, refresh. The dev server injects a
   `window.meshApp` that proxies the bridge ops to your local `my-corpus` index, so
   what you see is exactly what the desktop will show.

   Compose more from the SDK as you go (`import { … } from "../_sdk/meshapp.js"`):

   | Component | What it renders |
   |---|---|
   | `connect(corpus)` | the bound bridge client (`graph`/`node`/`subgraph`/`timeline`/`corpusStats`/`reconciliation`/`search`/`readChunk`) |
   | `scaleBanner` | the provenance/scale numbers |
   | `forceGraph` | the CSP-safe node-link graph |
   | `timelineChart` | monthly buckets (needs dated documents) |
   | `reconciliationList` | the identity-merge reveal |
   | `searchBox` / `threadList` / `barList` | search, on-ramp cards, degree bars |
   | `entityDetail` / `citedEdge` / `citationExpander` | the cited drill-down |

   `public/meshapp/enron/app.js` is the full worked example.

6. **Ship it** so consumers get one-click data. In `public/meshapp/my-explorer/`:
   - copy your `recipe.toml` into the bundle (it carries the `[prebuilt]` block);
   - add `corpus_data` to `meshapp.json`:

     ```json
     "corpus": "my-corpus",
     "corpus_data": { "size_indexed_gb": 0.4, "recipe": "recipe.toml" }
     ```

   That's it — the host discovers the app from its manifest (no code edit), and the
   desktop card offers **"Get data (0.4 GB) & Open."** See **MESHAPP_CONSUMER.md** for
   the experience your users get.

---

## Part C — publish to the curated registry

`meshapp dev` runs a bundle locally; the registry lets *others* install it.

7. **Pack + register** (self-contained: the bundle + a copy of `_sdk/`):

   ```bash
   sovereign meshapp publish my-explorer
   ```

   Writes `~/.svrnmesh/meshapps/artifacts/my-explorer-<v>.tar.zst`, prints its
   `sha256`, and records it in your local registry.

8. **Install** — fetch, verify, unpack:

   ```bash
   sovereign meshapp install my-explorer                    # from the registry
   sovereign meshapp install my-explorer --from <path|url>  # sideload
   sovereign meshapp list                                   # registry + installed
   ```

   Apps unpack under `~/.svrnmesh/meshapps/<id>/` (sharing one `_sdk/`). Install
   **verifies the sha256 and refuses a mismatch**. Run an installed app with
   `sovereign meshapp dev my-explorer`.

9. **Submit to the curated registry** so others get it (and one-click data):
   upload the tar to a URL / HuggingFace, then add the `[[apps]]` entry that
   `publish` printed to `sovereign-recipes/meshapp-registry.toml` via a PR. Being
   in that reviewed file is what marks an app **curated**.

### Trust & the security model

An installed bundle runs in a webview **with IPC access** (Tauri v2 doesn't gate
app commands per-window — tauri#9227). So the trust gate is:
- **integrity** — `install` verifies the artifact's `sha256` against the registry
  entry and refuses tampering;
- **curation** — membership in the reviewed `meshapp-registry.toml` (added by PR)
  is the trust anchor; `--from` sideloading warns that the app isn't curated.

Cryptographic signing (ed25519) is the next step. **True isolation for untrusted
apps** needs the *no-IPC bridge* milestone (a custom protocol / postMessage
instead of direct IPC); until then the curated registry IS the boundary — install
only apps you trust.

### Desktop integration (status)

The host enumerates installed apps via `meshapp_installed_apps()`. Opening an
installed third-party app **in the desktop's sandboxed window** — serving its
bundle from `~/.svrnmesh/meshapps/<id>/` via a registered `meshapp://` URI scheme
(first-party apps unchanged) — is the remaining integration; today installed apps
run via `sovereign meshapp dev <id>`.

---

## Two archetypes

- **Explorer** (what `meshapp new` scaffolds): an atlas graph — entities, relations,
  events, reconciliation, cited drill-down. Backed by the `[enrichment] type="atlas"`
  pass. UAP Blue Book and Enron are explorers.
- **Calculator** (e.g. SF-LVT): deterministic compute over typed atoms — every figure
  computed by the host and cited, no model originates a number. Uses the
  `readCorpus` / `parcelAnalytics` bridge ops over a tabular corpus.

Both read a corpus. A **ring app** writes one — see below.

## Ring apps — shared state instead of a shared corpus

`svrn ring` is the other half of the surface. An explorer or a calculator reads
data somebody already ingested; a ring app is state a *group* creates together —
a shared expense ledger, a tool-lending board, a chore rota. The unit of
deployment is the ring, not a host.

```
svrn ring new ./house-expenses            # the page, its reducer, its tests
svrn ring roster add alex --self --ring house-expenses
svrn ring dev house-expenses --dir ./house-expenses
svrn ring log house-expenses              # what is on the journal, in the terminal
```

Four things are worth knowing before you write one.

**The rail does not know what your app is about, and that is the deal.** It
carries an opaque payload — your JSON object — and guarantees exactly four
things about the acts it hands back: every one was signed by a key this ring's
roster claims; duplicates and equivocated ops are gone; corrections have been
applied and never resurrect; and the order is the same on every node. What an
act *means* is yours. The scaffold's `expenses.js` is that layer for the
reference app, with its own `node --test` suite beside it — keep the shape,
replace the rules.

**`window.ring` is three calls and a fold.** `ring.log()` returns the whole
namespace — the admitted acts, the gaps and the roster; `ring.record(payload)`
adds one signed act; `ring.correct(id, replacement?)` voids an earlier one,
permanently. Then `ring.fold(log, reducer, initial)` walks the acts in the
rail's order, skipping the ones a correction voided. **Use the fold rather
than iterating `log.ops`.** The guarantee is in the traversal: an app that
filters and sorts the array itself will double-count every corrected entry,
and it will look right until somebody makes a correction.

**A payload is a JSON object of whole numbers and strings.** No fractions —
`1e2`, `100.0` and `100` are one value with three spellings, and two nodes
have to derive identical bytes from your act in order to verify one
signature. Pick a unit and use an integer: cents, grams, milliseconds. The
rail refuses a fraction at the door and says so.

**`log.complete === false` is not a warning you may hide.** Your reducer's
answer is a function of the ops this node holds, and `gaps` is the honest
account of what it could not read — an op that has not arrived, a signature
that does not verify, a signer no roster claims. A book that shows a total
without saying it may be partial is worse than the spreadsheet it replaces.
The scaffold renders the gap panel for you — both the rail's gaps and your own
reducer's — and keeping it is not optional.

**The roster is not reachable from the app.** `svrn ring roster` writes it and
there is no rail route that can, so a deployed app cannot add a key to the ring —
including its own. Who is in the ring is a decision people make, not one an app
makes.

Two nodes converge on their own: each republishes everything it holds to every
online peer every sixty seconds, so a housemate who was offline for a week gets
the missing entries back, and one who leaves does not take their half with them.

## How it all fits (one source of truth)

The bridge ops live in the `sovereign-meshapp` library and are served identically by
the **desktop host** (Tauri commands) and the **`meshapp dev`** server — so your bundle
behaves the same in dev and in the shipped app. The app is sandboxed (strict CSP, no
network egress); its only host channel is the permission-gated `window.meshApp`.
