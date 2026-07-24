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

**Proof.**
- The mesh app runs against the **real** host bridge, not a mock: `meshapp_shim.js`
  over `__TAURI_INTERNALS__` → the real command bridge → the real daemon. (The
  existing synthetic specs mock this deliberately; the demo must not.)
- **Numeric honesty:** every headline number rendered in the scale banner is
  re-read from `meshapp_corpus_stats` over the bridge and compared. A demo that
  displays a number the backend doesn't report is a failed beat.
- The drill-down chunk resolves through `meshapp_read_chunk` to non-empty source text.
- Reconciliation surfaces ≥1 merge group with ≥2 surface forms.

**Preconditions.** `enron-sample-multi-wide` and `wikipedia-newsworthy` hosted
*with atlases built* (stats/graph/timeline ops require the atlas, not just chunks).

**Exports.** `b3-enron`, `b3-today`. GIF on the `reconciliation-reveal` mark.

---

### B4 — Atlas: explore your own notes on the commons

**Claim.** Point it at your Obsidian vault and it doesn't just search it — it
*reads* it. Entities, claims, the positions a text takes and what opposes them,
every one dereferenced back to the paragraph it came from. Your own thinking,
navigable.

**Choreography.**
1. Library → the vault notebook → **Explore**.
2. The atlas index: entities, events, claims, questions, positions, oppositions —
   with counts.
3. Search/scroll to **Elinor Ostrom**. Open the atom detail: the description the
   pass wrote *from the vault*, not from the model's priors —
   *"Nobel-winning economist whose career documented that real commons are not
   doomed by rationality."*
4. Walk the graph: the Nobel event, the states around it, the relation to the
   economics discipline that treated the finding as a curiosity.
5. Open a **Claim** about the commons and dereference it to the source passage.
6. "Ask about this" — carry the atom into a chat turn scoped to the vault.

**Proof.**
- The atlas surface renders ≥1 atom row and the atom detail is non-empty.
- The entity's on-screen description matches the atom record read over the bridge —
  the panel is showing the corpus, not paraphrasing it.
- The claim's evidence dereferences to a real chunk with non-empty text.
- The "Ask about" turn passes the full invariant pack and cites the vault corpus.

**Preconditions.** A vault corpus that is **hosted AND has a built atlas**.
`SOVEREIGN_DEMO_ATLAS_CORPUS` selects it; default `obsidian-vault`.

> **Known gap (2026-07-24).** On this machine the two halves are split: the
> hosted vault (`obsidian-vault-959ee8a8f330`) has chunks but no atlas, and the
> Ostrom-rich atlas (`obsidian-vault`) has no `chunks.lance` and so isn't hosted.
> The beat preflights both halves and **skips with that exact remediation**
> rather than filming half a surface. Fix by enriching the hosted vault
> (`sovereign enrich …`) — do NOT hand-overlay the old atlas onto the new corpus:
> the chunk ids are from a different ingest and the "dereferences to source"
> claim, which is the whole point of the beat, would silently be false.

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

### B7 — Lights out: the Pi (out-of-band)

**Claim.** This runs on a $80 computer with no internet. Cut the lights, cut the
network, it keeps answering.

**Status.** **Not Playwright-drivable** — the payload is physical (a Pi, a
switch, a dark room). This beat is captured by hand with the same encode
pipeline: record with `Cmd+Shift+5` (Show Mouse Pointer on, DND on), drop the
`.mov` at `test-artifacts/demo/raw/b6-pi-lights-out.mov`, and `demo:export`
processes it identically to the automated beats — same scale, same codec ladder,
same GIF settings — so it cuts into the reel without looking pasted in.

Framing to match the automated beats: 1280×800 at 2×, so the crop math in §5
applies unchanged.

**Proof.** Human. Stated as such rather than dressed up as a test.

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

---

## 3. Beat → surface → risk

| Beat | Surface | Hardest thing to keep honest |
|---|---|---|
| B1 | Ask + reading surface | Model may not emit an inline `[Source:]` marker → click-through unavailable |
| B2 | Inner Work | Witness prose is nondeterministic; only structure is assertable |
| B3 | Mesh apps | Requires built atlases, not just ingested chunks |
| B4 | Library → Explore | Atlas and chunks must belong to the SAME ingest, or drill-down lies |
| B5 | Workshop | Agentic authoring loop needs a capable primary |
| B6 | Mesh assist | Depends on another physical machine being up |
| B7 | Pi | Out-of-band by nature |
| B8 | Ask (code) | Retrieval must land on code, not on tests describing code |
| B9 | Library | Nothing — but the caption numbers must stay live-read |

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
but the desktop's data dir comes from the host `~/.sovereign/config.toml`
(that's how it reads the real corpora at all). So **the Library shelf is your
real shelf** — including anything test runs have left there.

Check it before filming B9's shelf pan and B4's notebook lookup:

```sh
ls ~/.sovereign/indexes | sed -n '1,200p'
```

Known passengers on this machine (2026-07-24), both planted by prior real-suite
attach runs and both visible on camera:

- `maple-house` — "Maple House (Governance Probe)", the governance fixture
- `folder-corpus-…` / `watched-…` — scratch folder ingests

Remove what you don't want in frame. `SOVEREIGN_DEMO=1` stops the capture run
from adding *more*, but it cannot retroactively clean what earlier runs left.

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
  raw/                # hand-recorded beats (B6) dropped in by the operator
  out/                # the ladder — mp4 / webm / poster / gif
  MANIFEST.md         # what got produced, what didn't, and why
```
