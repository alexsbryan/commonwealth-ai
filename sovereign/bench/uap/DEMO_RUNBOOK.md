# Demo Runbook — "What the Air Force could not explain"

**Audience:** open-source foundation / partners + the genuinely curious public
(UAP has broad reach; the credibility bar is *higher*, not lower).
**Length:** ~10–12 min. **Surface:** terminal (controllable) + one backing slide.
**One-line goal:** show that the real, public Project Blue Book archive — the U.S.
Air Force's own 17-year UFO investigation — can be **downloaded, structured into a
queryable evidence graph, and interrogated with grounded citations, locally, on a
laptop**, with a special light on the **701 cases the Air Force itself filed as
UNIDENTIFIED**. Nothing leaves the machine; every claim cites its source card.

> **The hook:** this isn't a believer's mixtape or a debunker's hit piece. It's the
> government's own record — 10,750 digitized case files, 126,000 scanned pages —
> turned into something you can *actually ask questions of*, with the answer always
> pointing back at the Air Force's own Form-10073 record card. The honest middle:
> here's what the record shows, including what it explicitly could not explain.

---

## The story (deliver this as the frame)

> From 1947 to 1969 the Air Force logged **12,618** UFO reports under Projects Sign,
> Grudge, and Blue Book. It explained the overwhelming majority — Venus, balloons,
> aircraft, birds, temperature inversions. But **701** it closed as **UNIDENTIFIED**:
> credible observers, often radar-confirmed, no conventional explanation found.
>
> Those files were declassified, microfilmed, and digitized — 126,000 scanned pages
> sitting in the National Archives. Searchable? Barely. Structured? Not at all.
> Watch what an afternoon — and a laptop — does to them.

---

## Provenance (say it up front — it's the credibility spine)

- **Source:** NARA's public AWS Open Data bucket (`s3://nara-national-archives-catalog`,
  record group 341) — no API key, no paywall, the government's own catalog.
- **Per-case, already segmented:** NARA describes Blue Book at the *fileUnit* (per-case)
  level, each with location + date + the scanned page images. **10,750** digitized cases.
- **The "Unknown" list:** the 701 (NICAP's published 558 + Brad Sparks' catalog),
  joined to the NARA cases to mark `is_unidentified`.
- **OCR:** the Air Force Form-10073 record cards (1950s microfilm) read locally with
  **PaddleOCR**, not a cloud service. Noisy but faithful — disposition, date, length
  of observation, and the brief summary are recovered straight off the card.

---

## Pre-flight (do this BEFORE the room)

1. **Daemon up, models loaded:** `sovereign daemon status` → embed + a 35B chat slot.
2. **Corpora installed:**
   - `uap-blue-book` — the **401 image-backed UNIDENTIFIED hero cases**, deep
     (OCR narrative + investigation graph). `~/.sovereign/indexes/uap-blue-book/`.
   - `uap-blue-book-index` — all **10,750 cases** as searchable metadata (breadth).
3. **CAPTURE the hero answers ahead of time** (35B synth is slow live). See §Capture.
4. **Backing slide:** the numbers from `CAPABILITY_BRIEF.md`.
5. **Fallback:** a pre-recorded screen capture of the full run. Never demo live without it.

---

## Act 1 — From 126,000 scanned pages to a structured archive *(the "before/after")*

**Show:** a raw scanned Form-10073 card (a JPG from NARA) next to the structured record
the pipeline pulled from it.

**Say:** "This is what the Air Force left us — a scanned microfilm card. The system read
it locally, pulled the date, the location, the length of observation, the conclusion
checkbox, and the summary, and filed it as a typed case. Times **10,750**. No cloud
touched it; the corpus is a single download."

**Run (install-in-seconds from the prebuilt snapshot):**
```sh
sovereign corpus install uap-blue-book        # pulls the prebuilt enriched index from HF
```
**They see:** the enriched corpus (graph + 401 hero cases) restored in seconds — *the
35B work already done, shipped, reproducible.*

---

## Act 2 — The 701: surface what stayed unexplained *(the hero)*

`[FILL POST-ENRICH]` — pick 3 of the strongest real UNIDENTIFIED cases from the enriched
graph (radar-confirmed, multiple witnesses, clean OCR). Candidates surfaced so far:
- **Iwo Jima, 24 June 1953** (BB-2605) — ground + radar, military witnesses.
- **Lake Charles, La., Aug 1952** (BB-1783).
- **Bohol Island, Philippines, May 1958** (BB-5800) — 90-sec falling object, smoke trail.

For each, show the **grounded, cited answer** + the **typed graph** (the sighting, the
observed object's shape/motion, the witness role, the installation, the adjudication =
UNIDENTIFIED), every claim pointing back at the Form-10073 card.

**Say:** "Ask it *what the Air Force could not explain near military installations in
1952* — and it doesn't hand-wave. It pulls the specific cases, the radar confirmation,
the witness roles, and shows you the actual card it's citing. This is the 701, legible."

---

## Act 3 — The hotspot map + the disposition bench *(turn "spooky" into "measured")*

**3.1 — Hotspots (descriptive geography, à la AARO's own maps).** `[FILL POST-ENRICH]`
The `sighting_hotspots` threshold over the unidentified set surfaces the installations
that recur most. Real bases coalesce across OCR spelling variants (Wright-Patterson /
WPAFB / "Aiforce" OCR noise → one installation) — show the merged node + its count.

**3.2 — Can a local model adjudicate like Blue Book did?** `[FILL POST-ENRICH]` Run the
disposition bench: the model classifies a case's disposition from its narrative, **era-
aware** (it can't say "Starlink" for 1952), scored against the Air Force's own ruling.
```sh
sovereign bench uap run --split test --policy tuned
sovereign bench uap diagnose --split test     # confusion matrix + worst confusions
```
**Say:** "We measured it. Here's the confusion matrix — and the mistakes are the *same*
ones the human investigators made: night-time astronomical-vs-aircraft, modern
satellite misreads. That's the tell that it's doing the real task, not pattern-matching."

---

## The close

"Fully local, on a laptop:
- **The government's own UFO archive** — 10,750 cases, 126,000 scanned pages — turned
  into a **queryable evidence graph**, downloaded in seconds.
- **The 701 it couldn't explain**, surfaced with their structured evidence and **every
  claim citing the Air Force's own record card**.
- **Nothing left the machine.** Public-domain source, local OCR, local inference,
  inspectable pipeline — and the corpus is one `sovereign corpus install` away for
  anyone who wants to dig in themselves."

---

## §Capture (run before the demo) `[FILL POST-ENRICH]`
Capture the 3 hero answers + their `[Source: …]` citations and the hotspot/bench numbers
from a real run once enrichment completes; paste into Acts 2–3.

---

## Honesty guardrails (so it survives Q&A)

- **"Unidentified" is the Air Force's own label, not a claim about reality.** It means
  *they* found no conventional explanation in *that* file — not "aliens." Say this plainly.
- **OCR is imperfect.** 1950s microfilm + form layout → noisy text; we show the raw card
  alongside so nothing is hidden. The substance (disposition, date, summary) is reliable;
  exact wording may garble.
- **Coverage is honest:** ~10,750 of the 12,618 cases were digitized by NARA; ~270 of the
  701 unknowns are matched to a digitized file with images (the rest are pre-1952
  Sign/Grudge-era or undigitized). State the numbers; don't imply totality.
- **The bench is single-corpus + small-N on the held-out split** — real, but don't
  over-generalize. Every number traces to `CAPABILITY_BRIEF.md` + the committed bench.
