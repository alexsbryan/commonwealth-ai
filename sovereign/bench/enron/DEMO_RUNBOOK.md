# Demo Runbook — "The Enron Task Force, in an afternoon"

**Audience:** open-source foundation / partners (mission- and credibility-driven, technically literate).
**Length:** ~10–12 min.
**One-line goal:** show that forensic-grade email investigation — identity resolution + grounded, cited answers — runs *locally, in seconds, on a laptop*, fully inspectable. The capability that costs a Fortune-500 legal team an e-discovery vendor and months of paralegals, democratized.

> **The foundation hook:** today this lives behind Relativity/Concordance + cloud AI — expensive, proprietary, your data leaves the building. We do it **sovereign** (nothing leaves the machine), **open** (every step inspectable, public-domain corpus), on **commodity hardware**. A journalist, a public defender, a watchdog — not just a megafirm — can run this.

---

## Surfaces — what you drive, and where each thing is shown

Two windows. Unlike a scanned-document demo, **Enron sources are text emails**, so the
desktop chat is *self-contained* — a citation popover shows the actual source email, no
browser tab needed.

| Window | Used for | Why (verified) |
|---|---|---|
| **Terminal** | Act 1 reconcile · the B³ measurement · the atlas-directed `enrich query` brief · the distillation counts | Reconciliation + B³ + atlas query are **CLI** and deterministic (no LLM) — fast and reproducible live. |
| **Sovereign Desktop app** | Act 2 grounded chat — ask a question, get a cited prose answer, click a citation → popover with the **actual source email** (subject + passage) | Grounded retrieval + citations is the desktop/server **Runtime** path (`SourceAttribution` + `SourcePopover`). The bare daemon's `/v1/chat/completions` is raw inference with **no retrieval**, so this is the app, not a CLI one-liner. |

> **Honest gaps to know:** the desktop **Atlas View** renders v2 atoms generically but is
> not tuned for this corpus's reconciliation/graph story — the **reconciliation + B³ +
> atlas-query** views are the terminal. Plan on terminal for Acts 1 & 3, desktop for Act 2.

---

## The story (deliver this as the frame)

> It's late 2001. Enron is collapsing. You're a federal investigator. On your desk: ~500,000 emails across ~150 custodians. The real Task Force spent **months** of paralegal time just figuring out *who is who* and *who said what* before the real analysis could begin.
>
> Watch what an afternoon looks like with this. We'll use a public slice — the mailboxes of **Ken Lay and Jeff Skilling**.

---

## Provenance (the credibility spine)

- **Corpus:** public-domain Enron email (EDRM / CMU set), the **Lay + Skilling sent+inbox
  slice** — **8,829 indexed chunks** after boilerplate stripping (signatures, quoted
  replies, disclaimers removed). 2 custodians, not the full 150.
- **No human in the loop:** extraction → resolve → reconcile runs from the raw mail.
- **Reconciliation is deterministic** — no LLM — so Act 1 and the B³ number are exactly
  reproducible. The QA answers (Act 2) are model synthesis over retrieved chunks.

---

## Pre-flight (do this BEFORE the room — and once as a full dry-run)

1. **Build** (so the bundled recipe + any fixes are current): `cargo build -p sovereign-cli-llm`.
2. **Daemon up, models loaded:** `sovereign daemon status` → expect an embed slot + a chat
   slot (this box runs the 35B `primary`). `curl -s localhost:9741/v1/models | head`.
3. **Corpus present:** `~/.sovereign/indexes/enron-sample-multi-wide/` with
   `atlas/atoms.json` (6,101 atoms) + `atlas/reconciliation.json`. Verify:
   `sovereign enrich reconcile enron-sample-multi-wide` → `1,730 → 35 merges` (see Act 1).
4. **Demo env (query decomposition + title expansion):**
   `export SOVEREIGN_TITLE_EXPAND=1 SOVEREIGN_DECOMP_DECAY=0.6`
5. **Desktop retrieval reachable:** launch the app (dev: `npm run dev` in
   `sovereign/crates/sovereign-desktop/`, then `tauri dev`; or the packaged `.app`). It
   attaches to the daemon on `:9741`. **Dry-run the Act 2 question** and confirm a citation
   popover shows the source email.
6. **CAPTURE the hero answers ahead of time** (35B synth is slow live). See **§Capture**.
7. **Fallback:** a pre-recorded screen capture of the full run. Never demo live without it.

---

## Act 1 — Reconstruct the cast *(LIVE — deterministic, ~15s)*

**Run:**
```sh
sovereign enrich reconcile enron-sample-multi-wide
```
**They see (verbatim):**
```
input entity atoms : 1730
canonical entities : 1689  (41 atoms collapsed into 35 multi-source clusters)
oplog merges       : 35
```

**Say:** "It read both inboxes and reconstructed the cast. From ~8,800 message chunks the
system distilled **1,730 real entities** — people, companies, places — and resolved their
cross-inbox aliases into canonical identities. `klay@enron.com`, `K. Lay`, `Kenneth L.
Lay` collapse to **one person**; `Calpine`, `Calpine Corp`, `Calpine Corporation` to one
company."

**The merges are real, named entities** (show `atlas/reconciliation.json`): Calpine
Corporation, El Paso Corporation, AES Corporation, Aquila Inc., Standard & Poor's, the
Midwest ISO — actual Enron counterparties, plus people like Annie R. Jones. **35
cross-inbox merges, 47.7% of surface forms collapsed.**

**The two lines that land with anyone legal:**
- **Precision = 1.0 on held-aside identities. Zero false merges.** "It never attributes one
  person's mail to another. When unsure, it leaves them apart — it misses a link before it
  invents one. That's the *only* admissible error direction for evidence." (Measured — Act 3.)
- **Glass-box:** open `atlas/reconciliation_oplog.jsonl` → "every merge carries its reason —
  shared email header, name fold, corroborated role. Defensible as an exhibit, not a black box."

> **The honest distillation story (this is a strength, lead with it):** raw entity
> extraction is *noisy* — it pulls document titles, dollar amounts, quoted fragments. The
> pipeline's **resolve** step distills that pile into **6,101 typed atoms** (1,730 entities
> + events, states, relations, claims, questions), and reconcile operates on the clean
> entities. So 35 merges of *named, real* counterparties beats "hundreds of merges" that
> were half document-titles. The number is smaller because it's **signal, not noise.**

---

## Act 2 — Follow the fraud *(grounded, cited — Sovereign Desktop)*

**Surface: the desktop app**, `enron-sample-multi-wide` selected. Ask live; the answer
streams in grounded prose, then a **"Sources:"** block — click a citation → a popover with
the **actual source email** (subject + the quoted passage). Because Enron sources are text,
the card *is* the email — no second window.

The five hero questions (`qa_demo.toml`), in narrative order:

1. **`exec_cast`** — *"Who were the senior executives running Enron, and what was each responsible for?"* (entity resolution → the cast: Lay/chairman, Skilling/CEO, Fastow/CFO)
2. **`ljm_fraud`** — *"What do these communications reveal about the LJM and Raptor partnerships, and which executive was behind them?"* (the fraud core)
3. **`dynegy_rescue`** — *"Describe the proposed Dynegy rescue of Enron as it appears in Kenneth Lay's communications to employees."* (the money shot — Lay's Nov-2001 mail)
4. **`financial_state`** — *"What was being said internally about Enron's financial condition — credit ratings, mark-to-market accounting, and off-balance-sheet exposure?"* (COMPOUND → query decomposition)
5. **`counterparty_network`** — *"Which energy companies appear as counterparties or competitors?"* (exercises the reconciled org entities)

**Lead with 2–4** (the strongest, captured answers in §Capture). **Say (on Q4):** "One
messy, three-part question. The system split it into focused sub-queries and chased each
thread — the S&P downgrade from one email, the equity adjustment from another — instead of
averaging them into mush and missing both. That's the query decomposition; we measured the
lift." *(→ Act 3.2.)*

> **Reproducible terminal alternative** (no GUI, deterministic, offline): the
> **atlas-directed** brief —
> ```sh
> sovereign enrich query enron-sample-multi-wide "LJM and Raptor partnerships and who ran them"
> ```
> returns a structured brief: `Entity: LJM … "related party previously managed by our
> chief financial officer" … controlled_by …` — i.e. it ties LJM to **Fastow** straight off
> the resolved graph. Good as a backup if the desktop isn't available, or to show the
> graph-grounded path beneath the prose.

---

## Act 3 — Show the work *(turn "magic" into "credible engineering")*

**3.1 — The reconciliation is measured, on identities it was never tuned against.**
```sh
sovereign bench enron run --corpus enron-sample-multi-wide --split test
```
**They see (verbatim):**
```
B³ precision/recall/F1 : 1.000 / 0.717 / 0.835   (n_aligned=10)
surface-form collapse  : 47.7%
delta vs pre-recon F1  : +0.220
merge signal histogram : name_similarity 35 · email_header 1
```
**Say:** "**Perfect precision — zero false merges — on a sealed test set.** Recall 0.717:
it surfaced ~72% of the true identity links; the third it misses are abbreviated names and
place-name variants — gaps that *widen the review net*, never *misattribute*. +0.220 F1
over the no-reconciliation floor. This is the number a prosecutor cares about."

**3.2 — Reconciliation + decomposition pay off in retrieval.** "A naive search for *Lay*
misses `klay@`, `K. Lay`, `chairman.ken@`. Because the system resolved them to one
identity, one question finds **everything** the man wrote. And it **decomposes** messy
multi-part questions into focused sub-queries — the broad question embedded whole buries
each fact below rank 100; decomposed, each sits at rank 2–11. We measured **+11 points** of
fact coverage." *(The same atlas-directed retrieval lifts buried counterparties: in the
counterparty question, atlas guidance moved gold recall from 1/5 → 3/5.)*

---

## The close

"To recap, fully local, on a laptop, in an afternoon:
- **Reconstructed the identity graph** — 8,800 message-chunks distilled to 1,730 real
  entities, cross-inbox aliases resolved at **perfect precision (B³ P=1.0)**, every merge
  auditable.
- **Answered the investigator's questions** in grounded prose, **each claim citing its
  source email**.
- **Nothing left the machine.** No cloud, no vendor, no data egress — sovereign by construction.

What the Enron Task Force did in months of paralegal review is a coffee break here — and
*more* defensible, because every link carries its evidence. The corpus is public-domain,
the pipeline inspectable, the methodology a committed benchmark."

---

## §Capture (run before the demo)

The reconcile + B³ + `enrich query` outputs are deterministic terminal output — re-run live
or screenshot. **Capture ahead** the **grounded chat answers** (35B synth is slow live):
```sh
export SOVEREIGN_TITLE_EXPAND=1 SOVEREIGN_DECOMP_DECAY=0.6
sovereign bench all --synth --isolate --filter enron/qa_demo
# writes the synthesized answers + cited sources to baselines/qa-synth-isolated/latest.json
```
Pull each answer + its `[Source: …]` citations into Act 2. The model on this box is the
35B (the prior captured prose was a 27B run — **re-capture fresh** so the on-screen answer
matches what you read). In the desktop app, screenshot the citation popover (the source
email) for each hero answer.

---

## Fallbacks

- **Daemon down / jetsam:** `sovereign daemon restart`, wait ~50s for models. Keep a
  **pre-recorded screen capture** of the full run as the ultimate backstop.
- **Latency too slow live:** pre-captured answers (default), or the offline `enrich query`
  brief, or force the 9B fast slot.
- **Strongest answers:** LJM/Raptor, Dynegy, financial-condition — grounded, cited,
  specific. Lead with these three.
- **`counterparty_network`** only reliably surfaces a few orgs (the known multi-entity
  recall gap). Use it *honestly* — "it tells you what it found and cites it, rather than
  confabulating the rest" — only if the room is technical.

---

## Honesty guardrails & anticipated skeptic questions

Lead with the framing; every number is reproducible (`sovereign enrich reconcile` /
`sovereign bench enron run`).

**Q: "1,730 entities, only 35 merges? That seems small."** It's small because it's
**distilled signal.** Raw extraction surfaces a noisy pile — document titles, dollar
amounts, quoted fragments. The resolve step distills 8,829 chunks → **6,101 typed atoms**
(1,730 entities); reconcile then makes 35 cross-inbox merges of *real, named* counterparties
(Calpine, El Paso, AES, S&P, Fastow's LJM…) at **P=1.0**. An earlier pipeline reported
hundreds of merges — but a sample showed ≥15% were document titles / filenames / fragments.
Fewer, named, zero-false-merge is the *stronger* forensic story.

**Q: "Precision 1.0 — isn't that just because you merged almost nothing?"** No: it's
measured on a **sealed test set** of canonical identities with B³ (cluster-level precision),
and recall is **0.717** — it's making the true links, not avoiding them. The error
asymmetry (miss before misattribute) is the only admissible one for evidence.

**Q: "Is this cherry-picked?"** The B³ is a **held-out test split** scored on a public
corpus with a committed methodology; reconciliation is **deterministic** (re-run it). A
second sealed holdout remains unspent for an independent generalization estimate.

**Q: "Does it scale past 2 custodians?"** Honest answer: **not yet measured at full scale.**
This is a 2-custodian slice (Lay + Skilling). The 500k-email / 150-custodian figure frames
the *matter*, not our run. The blocking is sub-quadratic, so per-custodian cost is *expected*
to hold — say "expected, not yet measured."

**Q: "The answers — is the model making them up?"** Every Act 2 answer is grounded in
retrieved chunks and **cites the source email**; click the citation to read it. The
atlas-directed `enrich query` path shows the same grounding without the LLM at all (entity →
relations → trajectory, straight off the resolved graph).

**Q: "Recall 0.717 — what's the missing third?"** Abbreviated names (`R. Mark`), place
variants (`Houston, TX` vs `Houston, Texas`), and entities the upstream text extractor never
surfaced. These *widen the net to review*, they don't misattribute — and the extractor gap
is a separate, known lever (a re-enrichment pass).
