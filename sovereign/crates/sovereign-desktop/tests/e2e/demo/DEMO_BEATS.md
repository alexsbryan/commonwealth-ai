<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Demo Beats — the product reel, as an executable spec

**Status:** harness built; beats encoded. Run: `npm run demo` → `npm run demo:export`.

This is the script for the product demo *and* the acceptance suite that proves
the script is not a lie. Every beat is a real-mode Playwright test driving the
real desktop app against a real daemon with real corpora and real models. If a
beat's correctness assertions fail, the clip is not exported — a demo we can't
verify is a demo we don't ship.

That coupling is the whole point. Marketing footage normally decays away from
the product silently. Here the reel is a test: when the product changes under
it, the run goes red before the footage goes stale.

---

## 1. Posture

| Decision | Choice | Why |
|---|---|---|
| Backend | **Attach mode** — the operator's live daemon on `:9741` | The demo needs the real corpora (`sep`, `enron-sample-multi-wide`, `wikipedia-newsworthy`, `commonwealth-ai`) and the real 35B primary. The managed fixture daemon has 3 toy documents and a 2B model. |
| Profile | Scratch `HOME` (`test-artifacts/demo-profile/`) | Conversations/config are scratch, so no personal thread history leaks into frame. Knowledge + inference are the real daemon's. |
| Knowledge bridge | Host `indexes/`, `recipes/`, `local-corpora/` symlinked in; tiered enrichment rows **projected** per corpus | The daemon's knowledge is half filesystem, half SQLite — and the SQLite half shares one file with the operator's 4,277 conversations, so it cannot be symlinked wholesale. `projectHostTieredMap()` copies the eight `corpus_id`-keyed tiered tables for `SOVEREIGN_DEMO_TIERED_CORPORA` only. Real enrichment output, relocated; personal state left behind. See §6. |
| Fixtures | **Skipped** (`SOVEREIGN_DEMO=1`) | The real suite plants "E2E Fixture Corpus" and "Maple House (E2E)" into the daemon index. Neither belongs on camera, and neither belongs in the operator's real index. |
| Capture | Playwright `recordVideo` at 2× | Deterministic framing across takes without hand-choreographing a cursor. See §5. |
| Cursor | Synthetic overlay, eased motion | Playwright's video has no OS cursor. The overlay is drawn in-page and moved with easing — the "cursor smoothing" a paid recorder sells, for free, and repeatable. |
| Failure | Beat fails ⇒ no clip | See above. |

**One test = one beat = one video file.** Playwright writes a video per test, so
beat boundaries are file boundaries and trimming needs no global clock. Sub-beat
*marks* inside a beat are recorded as offsets from the beat's own start, which is
what `demo-export.mjs` cuts the short-form GIFs on.

---

## 2. The beats

Each beat below states four things: **the claim** (what a viewer should walk away
believing), **the choreography** (what happens on screen), **the proof** (the
assertions that make the claim non-fictional), and **preconditions**.

---

### 2.1 Raw beats — footage a human shoots, a claim the machine still proves

Two beats cannot be filmed by Playwright: **B3**, because a mesh app only sees
real data inside its own labelled native window, and **B7**, because the machine
is a Raspberry Pi across the room. The obvious move — "drop a `.mov` in and run
it through the ladder" — would make those the one place in the reel where a claim
ships on trust. They are also the two most impressive clips, which is exactly
where a viewer's scepticism goes.

So a raw beat is still a test. `rawBeatTest` (in `beat.ts`) runs a **gate**
against the live daemon and the live app; `demo-export.mjs` encodes
`raw/<beat-id>.<ext>` **only** if that gate passed **in the same run**. A file in
`raw/` with no passing gate is refused and named in MANIFEST.md — "a human
dropped a file in" is not evidence, and the rest of the reel is held to evidence.
There is no override flag, for the same reason there isn't one for screencast
beats.

**The loop.**

1. `npm run demo -- --grep b3-enron` — the gate runs. On a pass it prints the
   recording guide and seeds `raw/b3-enron.captions.json`; on a skip it prints
   exactly what is missing and how to fix it.
2. Shoot the take. Save it as `raw/b3-enron.mov` (`.mp4`/`.m4v`/`.webm`/`.mkv`
   also work). **Record at 1280×800**, or any 16:10 — see below.
3. Fill in the cue sheet: `at` (seconds into the raw file) per caption, and
   optionally `trimInSec`/`trimOutSec` to cut the handles. A caption with
   `at: null` is skipped and reported, never guessed at.
4. `npm run demo:export`.

**One visual language.** A hand-shot clip that merely *shares a codec* with the
automated beats still reads as pasted in. So the exporter:

- **normalizes geometry** — scaled to fit and centred on the reel background at
  `REEL.width×REEL.height`, and re-timed to `REEL.fps`. A take at the wrong
  aspect is *framed*, not stretched, and the manifest says it was padded so you
  can reshoot;
- **burns the captions in using the same chip the live beats draw.**
  `reel-style.mjs` holds the geometry, the type and the chip once, and both
  renderers read it: `BeatRun.caption()` sets that CSS in the page, and the
  exporter rasterizes the *same* CSS through Chromium into an RGBA plate (with
  the app's own IBM Plex Sans Variable inlined, so the type cannot silently fall
  back). The frosted backdrop is the blurred frame carrying the chip's shape,
  which reproduces CSS `backdrop-filter` including the rounded corners.

  Verified rather than asserted: a burned-in caption measured **44.8 dB PSNR**
  against the live DOM render of the same frame, where the untouched video
  measured 41.2 dB against its own source. The caption survives the round trip
  better than the picture does.

  (Note for anyone tempted to simplify this to `drawtext`: this machine's ffmpeg
  build does not ship that filter at all.)

---

### B1 — "Is free will compatible with determinism?"

**Claim.** You can ask a hard question and watch the machine *work* — see what it
retrieved, see it decide, and follow the answer back to the sentence in the
Stanford Encyclopedia of Philosophy it came from. On your laptop.

**Choreography.**
1. Land on Ask. Scope the question to the SEP notebook via the corpus strip.
2. Type the question at human cadence — no instant `fill()`.
3. The glassbox opens: narration chips (retrieving, reading, weighing), then the
   synthesis heartbeat ticking a *token count* while the grounding gate holds the
   text back.
4. The answer streams in with inline `[Source: …]` markers.
5. Click a citation → the reading surface opens on the real SEP passage.
6. The epistemic footer settles: what it knew, what it didn't.

**Proof.**
- `assertTurnInvariants(requireCitations: true)` — concat(chunks) is byte-identical
  to `full_text`, exactly one terminal `message-complete`, no lagged stream.
- Every retrieved chunk resolves through `read_get_chunk` to real text. No dangling
  provenance.
- At least one citation carries `corpus_id === "sep"` — the answer is grounded in
  the corpus we claim, not in the model's memory.
- The synthesis heartbeat renders a **count**, never held content (`/\d[\d,]*\s+tokens?/`).
- The reading surface renders non-empty text from the clicked citation.

**Preconditions.** `sep` corpus hosted; primary model resident or loadable.

**Exports.** `b1-determinism` — hero clip (full beat). GIF cut on the
`citation-click` mark: the click-through to source, ~8s.

---

### B2 — Inner Work: an anxious journal entry, and the witness

**Claim.** There is a surface here that isn't a chatbot. You write; nothing is
sent anywhere; and when *you* ask for it, something reads you back with care.
This is the beat that makes people feel the product rather than evaluate it.

**Choreography.**
1. Rail → Reflect. The threshold holds the empty page. The dateline fades in.
   "Stored locally" sits in the corner, unblinking.
2. Type the entry — real cadence, with pauses at the paragraph breaks:

   > I keep circling the release. What if it isn't perfect. What if we put it in
   > front of people and it just — fails, in the ordinary way things fail, and
   > the failure is the thing they remember.
   >
   > Underneath that there's something I actually want to say. I want to build
   > this with people I've loved working with before. On work that's genuinely at
   > the edge. Where what we make matters to someone, and where we have real stake
   > in where it goes — not just a seat near the decision.
   >
   > I think the fear is just the size of wanting that.

3. `Cmd+Return` summons the witness. The composing dot pulses.
4. The witness response fades in as marginalia — not a reply, a reading.
5. Open the provenance panel: which model, where it ran, what left the machine (nothing).

**Proof.**
- `blockquote.witness` renders non-empty text.
- `.local-indicator` contains "local" and is visible for the whole beat.
- No uncaught page errors and no fatal Svelte diagnostics (inherited from the
  real-mode fixture — this is enforced, not assumed).
- The draft survives a rail round-trip (Reflect → Ask → Reflect) with the text
  intact — the persistence claim, proven rather than stated.
- Provenance panel names a model and reports local execution.

**Preconditions.** None beyond a resident chat model.

**Exports.** `b2-inner-work` — full clip. GIF cut on the `witness-appears` mark.

---

### B3 — Mesh Apps: the Enron task force, and today's news

**Claim.** A corpus isn't a chat log — it's a substrate other people can build
*applications* on. Same data, purpose-built lens, permission-gated, and every
pixel dereferences to a source document.

**Choreography.**
1. **Enron** (`enron-sample-multi-wide`): the scale banner — thousands of atoms,
   entities, reconciled merges. The force graph settles. Click an institution;
   the cited detail panel opens. Open the collapse timeline. Reveal reconciliation:
   "Calpine", "Calpine Corp.", "Calpine Corporation" — one company. Drill to a real
   email.
2. **Today** (`wikipedia-newsworthy`): the current-events lens over the same
   machinery, on your machine, with no feed and no server.

**Capture: RAW (§2.1).** A mesh app only sees real data from inside a window
labelled `meshapp-<app_id>`. `meshapp.rs authorize()` derives the calling app
from the webview label and is fail-closed on anything else; `command_bridge.rs`
always invokes as `MAIN_WINDOW_LABEL` ("main"). So every host op a bundle makes
through the test bridge is denied — including from a page Playwright navigated to
`/meshapp/enron/index.html` with the shipped shim injected, because the shim's
only transport **is** that bridge. Playwright's screencast is per-page and cannot
see the native window either. The operator records it; the gate proves it.

> An earlier version of this beat filmed the bundle in Chromium and could never
> have shown a real number. It only ever *skipped*, at a preflight that used the
> equally-gated `meshapp_corpus_stats` and read the denial as "no atlas". The
> other direction — teaching the test bridge to assert a mesh-app label — was
> considered and rejected: that label **is** the security boundary
> (`capabilities/meshapp.json` scopes the bridge commands to `windows:
> ["meshapp-*"]`), and a test-only hole through it is a production-shaped hole
> that exists so a demo can be convenient.

**What the gate proves** (`b3-meshapps.demo.spec.ts`):
- the corpus is hosted, and for Enron has a **built atlas** — read via the
  ungated `atlas_list_corpora`, the host-side reader of the same store;
- the app is **installed** and **granted** `mesh_store_read` (`meshapp_list_installs`);
- `meshapp_open(appId)` **succeeds** — the real labelled window builds, which is
  the one check the Chromium route could never make;
- for Today, freshness: the newest ingested day from `_doc_freshness.json` is
  within `SOVEREIGN_DEMO_FRESH_DAYS` (default 3). Deliberately **not** an atlas
  gate — Today resolves through `document_feed`, which reads documents, so
  `wikipedia-newsworthy`'s empty `atlas/` dir is correct and an atlas gate would
  skip a working app.

**Numeric honesty.** It was an assertion when this beat filmed the bundle; it
can't be now, so it is not quietly dropped. The gate writes the atlas counts into
the ledger and MANIFEST.md, and the recording guide says to check the scale
banner against them before keeping the take. Weaker than an assertion, much
stronger than a habit.

**Preconditions.** `enron-sample-multi-wide` hosted with an atlas built;
`wikipedia-newsworthy` hosted and freshly ingested; both apps installed. `enron`
ships in `public/meshapp/` but is **not** in the host's `[[meshapp_installs]]` —
installing it is itself a good on-camera gesture.

**Exports.** `b3-enron`, `b3-today`, from `raw/<id>.mov`. GIF cut on the first cue.

---

### B4 — Atlas: explore your own notes on the commons

**Claim.** Point it at your Obsidian vault and it doesn't just index it — it
*reads* every note into a summary tree, with the entities it found, on your
laptop; and every cluster is built from paragraphs still in the index.

**Choreography.**
1. Library → the vault notebook → **Explore**.
2. The note map: every note in the vault, read — with its state, chunk count and
   salience-ranked entity chips.
3. Type **Ostrom** into the note search; the list narrows to the anchor note.
4. Open it: the summary tree — level-0 topic clusters, each with the summary the
   enrichment pass wrote, its coherence and its evidence count.
5. Park on the money frame and let the summary be read.

**Proof.**
- The Explore tab mounts the tiered note map (`.conv-corpus-view`) and renders
  ≥1 note row.
- The anchor note has ≥1 *substantive* cluster (not synthetic-tiny, summary
  >80 chars) — otherwise the pane renders "too short to break into topic
  clusters", a true sentence over an empty frame, and the beat skips.
- The summary on screen matches one `atlas_get_conv_detail` returned — the panel
  is showing the stored tree, not paraphrasing it.
- **Dereference gate (off-camera).** This surface shows an evidence *count* with
  no click-through, so provenance is machine-checked instead of filmed: a sample
  of the clusters' member chunk ids must each resolve, through *this* corpus, to
  non-empty text (`read_get_chunk`). That is what catches a tree built against a
  different ingest. The sample cap is reported, never silent.

**Preconditions.** A vault corpus with a **tiered (System 3) map the desktop can
see**, that is **on the shelf as an explorable notebook**, holding a note whose
title matches the anchor. `SOVEREIGN_DEMO_ATLAS_CORPUS` /
`SOVEREIGN_DEMO_ATLAS_ANCHOR` select them; defaults
`obsidian-vault-959ee8a8f330` / `Ostrom`. Each precondition skips with its own
remediation — they have different fixes.

> **What this beat films, and does not.** An Obsidian vault is a **System 3**
> corpus *by design* (`corpus-engine/ENRICHMENT.md`: the System-2 `obsidian_atlas`
> pipeline was removed when vaults moved onto the tiered path, and System 3 is
> called the gold standard for user-facing corpora). So the surface here is the
> RAPTOR note map, not the typed atom graph — those two things share the word
> "atlas" and **do not interoperate**. Do not reach for `sovereign enrich
> init|build` to fix a skip: that builds the atom map, a different artifact on a
> different surface. What it costs, stated plainly: no entity/claim/opposition
> graph and no click-through to a source passage, because System 3 renders
> neither.

> **Known trap (2026-07-25).** In attach mode the desktop reads its OWN tiered
> store at `config.data_dir/sovereign.db`, and a baked profile's `data_dir` is
> the scratch default — the daemon's map at `~/.svrnmesh/sovereign.db` is
> invisible to it, because none of the six `atlas_*conv*` commands has an
> attach-mode branch and the daemon exposes no atlas route to branch to. (The
> operator's real install is unaffected: it walked the setup flow, which adopts
> the CLI's `[data] dir`.) Global setup bridges it by projecting the daemon's
> tiered rows for the filmed corpus into the scratch store — §1 Posture. The
> gate reads the *bridge*, never the daemon's sqlite, precisely so a projection
> that did not land skips the beat instead of filming an empty tree.

---

### B5 — Workshop: author a workflow by talking to it

**Claim.** You are not limited to what we shipped. Describe the job in English;
the system writes the recipe, shows you the TOML it wrote, validates it, and runs
it. The maker surface is in the product, not in a docs site.

**Choreography.**
1. Rail → Workshop → Build. New project.
2. Describe the workflow in the authoring chat.
3. Toggle the TOML view — the generated recipe, readable, editable, yours.
4. Validation passes.
5. Switch to Run: pick the workflow, fill the params, run it, watch the step
   progress complete.

**Proof.**
- `recipe-author-toml-editor` contains parseable, non-empty TOML.
- `recipe-author-toml-errors` is absent/empty — the authored recipe validates.
- A run reaches `workflow-run-complete` (not `workflow-run-failed`) with a
  non-empty step list.

**Preconditions.** A capable primary. The authoring loop is agentic; the 2B
profile cannot drive it (this is why the real suite gates
`real-workflow-author.spec.ts`). Attach mode against the 35B satisfies it.

**Exports.** `b5-workshop`. GIF on the `toml-reveal` mark.

---

### B6 — Borrow a bigger brain from the mesh

**Claim.** The laptop is not the ceiling. A machine you trust, on your mesh, lends
you its GPU — and the provenance says exactly whose, running what. Sovereignty
that scales past one device.

**Choreography.**
1. Mesh status: members online, pooled VRAM.
2. Ask something that deserves the big model. Offer the mesh assist.
3. Route to the peer (RuggedFox) running the 122B.
4. The assist progress panel shows the work moving.
5. Provenance names the peer node and the model — the receipt.

**Proof.**
- Bridge mesh status reports the target peer **online** before the beat runs
  (otherwise: skip, loudly — see §4).
- The turn's provenance/metadata names a **remote** execution site whose node id
  matches a peer from mesh status — not the local node.
- `assertTurnInvariants` still holds across the remote turn (stream integrity is
  not relaxed because the compute moved).

**Preconditions.** Peer online, the large model resident there. **This beat is
skipped, not faked, when the peer is down.**

**Exports.** `b6-peer-compute`. GIF on the `provenance-receipt` mark.

---

### B7 — The Pi writes the code

**Claim.** The model on the Pi doesn't autocomplete — it reads a spec, writes a
program, and the held-out tests it never saw pass. On a $80 computer, with no
cloud.

**Capture: RAW (§2.1).** Physics, not architecture: the machine in frame is
across the room and no browser automation reaches it.

**What the gate proves** (`b7-pi-coding.demo.spec.ts`), against the battery's own
`--report` JSON:

```bash
sovereign agent-bench run --problems 3.2 \
  --report sovereign/crates/sovereign-desktop/test-artifacts/demo/raw/b7-pi-coding.bench.json
```

- the report is **recent** (≤ `SOVEREIGN_DEMO_BENCH_MAX_AGE_DAYS`, default 14) —
  a receipt from three months ago does not describe the build being filmed;
- the run **completed** on its own terms: `exit_reason.kind == "completed"` and
  not scored partial (a timeout or token-budget kill is not this claim);
- the **held-out fixtures** are green: `witness_summary.verify_exit_ok`,
  `failed == 0`, `passed > 0`;
- **correctness is absolute** — `dim_a` (auto-scored from those fixtures) must be
  a full 3/3. The claim is that the program *works*, not that it nearly worked;
- **total ≥ 7/9** (`SOVEREIGN_DEMO_BENCH_FLOOR`). Correctness alone can pass while
  approach (`dim_b`: GF(2) vs brute force) and efficiency (`dim_c`) are weak, and
  the clip implies a good solution rather than merely a passing one.

`3.2-lights-out` is the hardest tier the battery ships, which is the point: a
passing receipt is worth showing. Override the problem with
`SOVEREIGN_DEMO_BENCH_PROBLEM` if you film a different one.

**Proof.** The claim is machine-checked; the pixels are human-attested. Both are
stated on the record in MANIFEST.md rather than blurred together.

---

### B8 — Ask your codebase a question you can't grep for

**Claim.** Point it at a repo and ask the question you'd ask a senior engineer —
in English, naming no symbol — and get the actual code path back. The subsystem
you're afraid to touch becomes one you can change. And the code never leaves the
laptop. (Thesis and answer-key: `sovereign/docs/specs/CODE_INTEL_CHAT.md`.)

**Choreography.**
1. Scope to the `commonwealth-ai` notebook.
2. Ask, naming no function: *"When a chat turn needs a bigger model than this
   laptop can run, how does the request actually get to another machine and come
   back?"*
3. The answer comes back with real file paths, real functions, and a call-graph
   trace appended to the evidence.
4. Click through to the source line.

**Proof.**
- Citations resolve and ≥1 carries `corpus_id === "commonwealth-ai"`.
- The answer text contains at least one real repo path (`/\b[\w/-]+\.rs\b/`) that
  **exists on disk** — the anti-hallucination gate for this beat, and the one that
  matters most to the audience.
- The "pop" constraint is enforced at the *spec* level: the question string is
  asserted to contain no symbol from the expected answer set. If someone later
  "fixes" the demo by naming the function, the test fails.

**Preconditions.** `commonwealth-ai` corpus hosted with `scip_graph.db` present.

**Exports.** `b8-code-intel`. GIF on the `code-answer` mark.

---

### B9 — The shelf, and the ask

**Claim.** Everything you just saw is one shelf in a library you own, and we've
barely started. Come build it with us.

**Choreography.** The Library, scrolling. Then a caption over it.

**Proof — and this is the point of the beat.** The numbers in the closing caption
(*N notebooks · M chunks searchable · P machines · Q GB pooled VRAM*) are read
live from the daemon at capture time and injected into the overlay. They are not
typed into a design file. If the caption says it, the machine reported it.

**Exports.** `b9-shelf` — outro clip.

### B10 — Deep research: a question, a budget, a checked report

**Claim.** Ask with a budget and a typed release; watch the rounds, the
gate's named gaps, and the budget ledger live; read the checked report
with its verdicts — every `[passed]` figure traced in the evidence.

**Choreography.** Chat empty state → Deep research. The Ask entry: the
question typed, rounds/search/fetch budget, the estate-first corpus
chips, and the consent grant — default-deny standing, then a typed
public-web release. Start the run; the live view renders the round, the
gate's gap list, the budget ledger, and the consent-grant status — all
read from the run dir the verb writes. The run closes; the report view
renders the verb's own checked report: claims with verdict + corroboration
accounting, residue, reframe, and the constitution position. Find it in
Library.

**Proof.**
- The live view's stage/round/run-id, meters (`N spent` / `N remaining`),
  and consent status appear **after** the verb names its run dir — the
  desktop renders the run-dir artifacts, never a second state source.
- The report view's question and body are the verb's artifacts.
- The constitution position (order t3b's (g)): `dr-constitution` reads
  "Position holds — N [passed] claims, every figure traced in the
  evidence" or the honest zero-claim variant; `dr-constitution-violations`
  is **absent** — a named violation turns the beat red.
- The Library handoff: `Find it in Library` lands on the Library surface.

**Determinism.** The run is served from the bank v1 report-class deck
(`SOVEREIGN_DEMO_DR_FLAGS` → `--backend mock --mock-deck <deck>`, set in
demo global-setup, declared in `quality/env-flags.toml`): search/fetch
resolve against the deck, drafts still run on the real daemon. The deck
is the single evidence source, so the constitution check is asserted for
real. Consent honesty: the typed public-web grant is recorded in the
run's charter; the deck serves every hit, so nothing leaves the machine —
the caption says exactly that.

**Exports.** `b10-deep-research`. GIF on the `report-ready` mark.

---

## 3. Beat → surface → risk

| Beat | Surface | Hardest thing to keep honest |
|---|---|---|
| B1 | Ask + reading surface | Model may not emit an inline `[Source:]` marker → click-through unavailable |
| B2 | Inner Work | Witness prose is nondeterministic; only structure is assertable |
| B3 | Mesh apps | Unfilmable by Playwright (§2.1); the numbers on screen are checked by a human against gate-printed atlas counts |
| B4 | Library → Explore | Atlas and chunks must belong to the SAME ingest, or drill-down lies |
| B5 | Workshop | Agentic authoring loop needs a capable primary |
| B6 | Mesh assist | Depends on another physical machine being up |
| B7 | Pi | Hand-recorded by physics (§2.1); the claim rides on the bench report, not on the footage |
| B8 | Ask (code) | Retrieval must land on code, not on tests describing code |
| B9 | Library | Nothing — but the caption numbers must stay live-read |
| B10 | Ask → Deep research → Library | A real run is minutes long; the constitution position must hold against the deck-served evidence |

---

## 4. Skips are loud, never silent

A beat with unmet preconditions is **skipped with a stated reason**, recorded in
the ledger, and printed in the run summary. It is never quietly downgraded to a
mocked version, and its clip is never exported from a previous run. The reel
either has the beat or visibly doesn't.

`demo:export` prints a manifest of what it produced *and what it couldn't*, so
"the peer was down that day" is a fact you read off the run, not a thing you
discover in the edit.

---

## 5. Capture geometry & the encode ladder

**Geometry.** Viewport `1280×800`, `deviceScaleFactor: 2`, video `2560×1600`.
Fixed across every beat, which is most of what makes a multi-clip reel look
intentional. Because capture is at 2×, a post-hoc crop to a region is a *real*
zoom at full 1× output resolution — no upscale artifacts:

```sh
ffmpeg -i raw.mp4 -vf "crop=1600:1000:480:300" -c:v libx264 -crf 20 zoomed.mp4
```

The *small* viewport is the readability lever, not a font hack: 1280 logical
pixels of app in a 2560px frame means every glyph is twice the size it would be
in a full-screen capture, which is what survives an 800px embed. Override with
`SOVEREIGN_DEMO_WIDTH` / `SOVEREIGN_DEMO_HEIGHT` / `SOVEREIGN_DEMO_SCALE` if a
beat needs a tighter frame — but change it for the *whole* run, never per beat,
or the reel stops cutting together.

**Ladder** (all emitted by `demo:export`, per beat):

| Artifact | Use | Notes |
|---|---|---|
| `<beat>.mp4` | site/video embed | H.264 CRF 26, `yuv420p`, `+faststart`, `-an` |
| `<beat>.webm` | first `<source>` | VP9 CRF 40, `-an` |
| `<beat>-poster.webp` | `poster=` | first frame |
| `<beat>.gif` | README / social | 15fps, 800px, gifski `-Q 80`; ffmpeg palettegen fallback |

`-an` strips audio outright (a muted video still ships the bytes otherwise);
`+faststart` moves the moov atom to the front so playback starts before download
finishes.

Embed:

```html
<video autoplay loop muted playsinline preload="metadata" poster="b1-determinism-poster.webp">
  <source src="b1-determinism.webm" type="video/webm">
  <source src="b1-determinism.mp4" type="video/mp4">
</video>
```

**GIF budget.** 8–10s. Over ~5 MB, drop to 12fps or 640px *before* sacrificing
quality — length is always the cheapest thing to cut. `demo:export --gif-max-mb`
enforces this and re-encodes down the ladder automatically rather than shipping a
9 MB GIF.

---

## 6. Before you shoot

Demo mode isolates *conversations and desktop config* into a scratch profile,
but global setup symlinks the host's `~/.svrnmesh/{indexes,recipes,local-corpora}`
into it (that's how it reads the real corpora at all). So **the Library shelf is
your real shelf** — including anything test runs have left there.

Check it before filming B9's shelf pan and B4's notebook lookup:

```sh
ls ~/.svrnmesh/indexes | sed -n '1,200p'
```

Known passengers on this machine (2026-07-24), both planted by prior real-suite
attach runs and both visible on camera:

- `maple-house` — "Maple House (Governance Probe)", the governance fixture
- `folder-corpus-…` / `watched-…` — scratch folder ingests

Remove what you don't want in frame. `SOVEREIGN_DEMO=1` stops the capture run
from adding *more*, but it cannot retroactively clean what earlier runs left.

**The tiered map is projected, not shared.** The other half of the daemon's
knowledge — RAPTOR trees, skeletons, entity mentions, vault themes — lives in
`~/.svrnmesh/sovereign.db`, the same file as the operator's 4k conversations, so
it is copied per corpus rather than symlinked (§1 Posture, B4's known trap).
`SOVEREIGN_DEMO_TIERED_CORPORA` is the comma-separated whitelist; it defaults to
`SOVEREIGN_DEMO_ATLAS_CORPUS`, so pointing B4 at a different vault carries its map
across automatically. Setup logs the row counts it projected — if a beat that
needs a map skips, read that line first:

```
[real-setup] demo: projected daemon tiered map for {obsidian-vault-…} → …/sovereign.db
[real-setup]   conv_skeletons=322 conv_raptor_nodes=606 vault_themes=2 chunk_entities=19819
```

---

## 7. Running it

```sh
sovereign daemon start                 # attach target: real corpora, real models
npm run demo                           # capture (fails loudly on unmet preconditions)
npm run demo -- --grep b1              # one beat
npm run demo:export                    # trim + encode + gif, prints the manifest
npm run demo:export -- --beat b1-determinism
```

Artifacts land in `test-artifacts/demo/`:

```
test-artifacts/demo/
  ledger.jsonl        # one record per beat: status, marks, video path, skip reason
  video/              # raw Playwright webm, one per beat
  raw/                # RAW beats (§2.1) — the operator's takes
    <beat>.mov            the take itself
    <beat>.captions.json  cue sheet: caption times + trim handles (gate-seeded)
    b7-pi-coding.bench.json  B7's receipt from `sovereign agent-bench run --report`
    .master/              normalized + captioned intermediates the ladder encodes
  out/                # the ladder — mp4 / webm / poster / gif
  MANIFEST.md         # what got produced, what didn't, and why
```

Useful knobs on the exporter beyond §5's: `--fresh-masters` re-renders raw
intermediates after a cue-sheet edit, `--no-caption-blur` drops the frosted
backdrop behind burned-in captions, and `SOVEREIGN_DEMO_ARTIFACTS=<dir>` points
the whole thing at a scratch tree so you can exercise it without touching the
real ledger.
