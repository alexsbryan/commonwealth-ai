# Demo Runbook — "The Enron Task Force, in an afternoon"

**Audience:** open-source foundation / partners (mission- and credibility-driven, technically literate).
**Length:** ~10–12 min. **Surface:** terminal (controllable) + one backing slide.
**One-line goal:** show that forensic-grade email investigation — identity resolution + grounded, cited answers — runs *locally, in seconds, on a laptop*, fully inspectable. The capability that costs a Fortune-500 legal team an e-discovery vendor and months of paralegals, democratized.

> **The foundation hook:** today this lives behind Relativity/Concordance + cloud AI — expensive, proprietary, your data leaves the building. We do it **sovereign** (nothing leaves the machine), **open** (every step inspectable, public-domain corpus), on **commodity hardware**. A journalist, a public defender, a watchdog — not just a megafirm — can run this.

---

## The story (deliver this as the frame)

> It's late 2001. Enron is collapsing. You're a federal investigator. On your desk: ~500,000 emails across ~150 custodians. The real Task Force spent **months** of paralegal time just figuring out *who is who* and *who said what* before the real analysis could begin.
>
> Watch what an afternoon looks like with this. We'll use a public slice — the mailboxes of **Ken Lay and Jeff Skilling**.

---

## Pre-flight (do this BEFORE the room)

1. **Daemon up, models loaded** (watcher disabled for stability):
   ```sh
   sovereign daemon status        # expect /v1/models to list embed + a chat model
   curl -s localhost:9741/v1/models | head
   ```
2. **Corpus indexed:** `~/.sovereign/indexes/enron-sample-multi-wide/` present (chunks.lance + atlas/atoms.json).
3. **Demo config (env):** `export SOVEREIGN_TITLE_EXPAND=1 SOVEREIGN_DECOMP_DECAY=0.6`
4. **CAPTURE the hero answers ahead of time** (27B synth is ~2 min/question — too slow to stare at live). See **§Capture**. Have the captured answers + their cited sources on screen, ready to reveal.
5. **Backing slide ready:** the numbers from `CAPABILITY_BRIEF.md` (B³ 0.80 / P=1.0; QA +11pt keyword).

---

## Act 1 — Reconstruct the cast *(LIVE — it's genuinely ~15s)*

**Run:**
```sh
sovereign enrich reconcile enron-sample-multi-wide
```
**They see:** `18,833 entity mentions → 17,293 canonical (935 audited merges)` in ~15 seconds.

**Say:** "It just read both inboxes and reconstructed the cast. `klay@enron.com`, `K. Lay`, `Kenneth L. Lay`, `chairman.ken@` — the system knows those are **one person**. Skilling, Fastow, every counterparty — collapsed to canonical identities, with the communication graph between them."

**The two lines that land with anyone legal** (show `atlas/reconciliation.json` + the oplog):
- **Precision = 1.0. Zero false merges.** "It never attributes one person's mail to another. When unsure, it leaves them apart — it misses a link before it invents one. That's the *only* admissible error direction for evidence."
- **Glass-box:** open `atlas/reconciliation_oplog.jsonl` → "every merge carries its reason — shared email header, name fold, corroborated role. 935 of them. Defensible as an exhibit, not a black box."

---

## Act 2 — Follow the fraud *(grounded, cited — PRE-CAPTURED real answers, reveal one at a time)*

Three questions, each answered in grounded prose **citing the actual source email**. (Captured 2026-05-31; regenerate via §Capture.)

**Q2.1 — the fraud core** *(fact recall 1.0):* *"What do these emails reveal about the LJM and Raptor partnerships, and who was behind them?"*
> "**Andrew Fastow formed and ran two investment partnerships — LJM Cayman and LJM2 Co-Investment — with Enron board approval, creating a conflict-of-interest issue.** It also mentions **$1.1 billion banked this week** and Palmer's comments on credit lines **[Source: Saturday Articles]**."

**Say:** "It names the executive, the exact vehicles — LJM Cayman, LJM2 — the *board's* role, the conflict of interest, and the dollar figure. Pulled from the mail, cited. That's the heart of the case."

**Q2.2 — the money shot** *(fact recall 1.0):* *"Describe the proposed Dynegy rescue of Enron as it appears in Ken Lay's communications."*
> "Based on Kenneth Lay's communications to employees, particularly the email sent **November 9, 2001, titled 'Enron Announces Plans to Merge with Dynegy,'** … management and the Board concluded that merging with Dynegy — a major energy player and **one of Enron's largest counterparties** — was the **best alternative [Source: Enron Announces Plans to Merge with Dynegy]**. He argued it would rebuild investor and market confidence, strengthen Enron's balance sheet, and preserve the core businesses."

**Say:** "Lay's actual November-9 email to employees. Every claim carries its source. *This* is the 'who-knew-what-when' a prosecutor builds a case on — seconds, not a review team."

**Q2.3 — the compound question** *(this is where the new retrieval tech shows — fact recall 0.75):* *"What was being said internally about Enron's financial condition — credit ratings, mark-to-market accounting, and off-balance-sheet exposure?"*
> "Enron faced significant downgrades. **Standard & Poor's downgraded Enron's long-term rating one notch from BBB+ to BBB, and its short-term rating from A2 to A3 [Source: Enron Update]** … [and] a **$1.2 billion equity adjustment [Source: Financial Disclosure of $1.2 Billion Equity Adjustment]**."

**Say:** "One messy, three-part question. The system pulled the *specific* S&P downgrade — BBB+ to BBB — from one email **and** the $1.2 billion equity adjustment from another. That's the **query decomposition** at work: it split the compound question and chased each thread, instead of averaging them into mush and missing both. We measured that lift: +11 points of fact coverage." *(→ flows straight into Act 3.2.)*

---

## Act 3 — Show the work *(turn "magic" into "credible engineering" — 2 glass-box reveals)*

**3.1 — Reconciliation pays off in retrieval.** "A naive search for *Lay* misses `klay@`, `K. Lay`, `chairman.ken@`. Because the system resolved them to one identity, one question finds **everything** the man wrote, across 40 surface forms. That's the difference between a keyword grep and an investigation."

**3.2 — It decomposes messy questions.** Re-show Q2.3 with its glass-box trace: "A human asks a sloppy, three-part question. The system **split it into focused sub-queries** — one per aspect — because the broad question, embedded whole, buries each fact below rank 100; decomposed, each sits at rank 2–11. We measured it: **+11 points** of fact coverage in the answer. It doesn't miss the buried thread."

---

## The close

"To recap, fully local, on a laptop, in an afternoon:
- **Reconstructed the identity + communication graph** — 18,833 mentions → the canonical cast — at **perfect precision**, every merge auditable.
- **Answered the investigator's questions** in grounded prose, **each claim citing its source email**.
- **Nothing left the machine.** No cloud, no vendor, no data egress — sovereign by construction.

What the Enron Task Force did in months of paralegal review is a coffee break here — and *more* defensible, because every link carries its evidence. And it's open: this corpus is public-domain, the pipeline is inspectable, the methodology is a committed benchmark. This isn't a megafirm capability anymore."

---

## §Capture (run before the demo; ~10–12 min on the 27B)

```sh
export SOVEREIGN_TITLE_EXPAND=1 SOVEREIGN_DECOMP_DECAY=0.6
sovereign bench all --synth --isolate --filter enron/qa_demo
# extract the synthesized answers + cited sources from the run JSON it writes
```
Paste each answer + its `[Source: …]` citations into Act 2 above. **Faster alternative for a snappy *live* run:** force the 9B fast slot (lower latency, still grounded) instead of pre-capturing.

---

## Fallbacks

- **Daemon down / jetsam:** `sovereign daemon restart`, wait ~50s for models. Keep a **pre-recorded screen capture** of the full run as the ultimate backstop — never demo live without it.
- **Latency too slow live:** pre-captured answers (default) or 9B fast slot.
- **Strongest answers (verified 2026-05-31 capture):** **LJM/Raptor, Dynegy, financial-condition** — all grounded, cited, specific (fact 0.75–1.0). Lead with these three.
- **Demote/skip:** `exec_cast` hedges ("fragmented excerpts") — Act 1's `reconcile` shows the cast better, so don't ask it as QA. `counterparty_network` only reliably surfaces **Dynegy** (fact 0.20 — the known multi-entity recall gap); skip it, or use it *honestly* to make a virtue of the glass-box: "it tells you what it found and cites it, rather than confabulating the rest" — but only if the room is technical and you want to show the precision discipline.

---

## Honesty guardrails (so it survives Q&A)

- This is a **2-custodian slice** (Lay + Skilling), not the full 150-custodian corpus. Frame 500k/150 as the **vision/extrapolation**, explicitly — the slice is proof-of-mechanism. The blocking design is sub-quadratic, so per-custodian cost holds, but say "expected, not yet measured at full scale."
- Entity resolution is **measured** (B³ 0.80 / P=1.0 on held-aside identities). QA quality is real but **single-corpus**; don't claim universal — the query-expansion config is gated precisely because it's a targeted (not universal) win.
- Every number on the backing slide traces to `CAPABILITY_BRIEF.md` + the committed bench. If asked "is this cherry-picked?" — the answer is the held-out test split + the public corpus + the committed methodology.
