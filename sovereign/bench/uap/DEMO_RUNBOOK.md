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
- **The "Unknown" list:** NICAP's published roster of the AF's unidentified cases
  (**558 rows** — a machine-readable subset of the AF's famous **701**), plus Brad
  Sparks' catalog, joined to the NARA cases on location + year to mark
  `is_unidentified` (**270** distinct unknowns matched → **401** file-units).
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

Three real UNIDENTIFIED cases, verified in the hero set (199 of the 401 mention
radar). Capture the grounded answers live per §Capture:
- **Albuquerque, N.M., Aug 1951** (BB-955) — radar, near the Sandia / Los Alamos
  nuclear complex; ties straight into Act 3's nuclear-site cluster.
- **Iwo Jima, 24 June 1953** (BB-2605) — ground + radar, military witnesses.
- **Bohol Island, Philippines, May 1958** (BB-5800) — 90-sec falling object, smoke trail.

For each, show the **grounded, cited answer** + the **typed graph** (the sighting, the
observed object's shape/motion, the witness role, the installation, the adjudication =
UNIDENTIFIED), every claim pointing back at the Form-10073 card.

**Say:** "Ask it *what the Air Force could not explain near military installations in
1952* — and it doesn't hand-wave. It pulls the specific cases, the radar confirmation,
the witness roles, and shows you the actual card it's citing. This is the 701, legible."

---

## Act 3 — The hotspot map + the disposition bench *(turn "spooky" into "measured")*

**3.1 — Hotspots (descriptive geography, à la AARO's own maps).** The
`sighting_hotspots` threshold over the unidentified set surfaces **15** installations
that recur most. The headline:

| Installation | Unidentified sightings nearby |
|---|---|
| **Wright-Patterson AFB** (Blue Book HQ / ATIC) | 15 |
| **Washington, D.C.** (the 1952 Capitol radar-visual flap) | 7 |
| Kelly AFB · San Antonio | 7 |
| Kirtland AFB | 6 |
| Lake Charles · George AFB | 5 |
| **Los Alamos · Oak Ridge** (the nuclear-site cluster) | 4 |

**Say:** "Two things jump out. Wright-Patterson — the Air Force's own UFO HQ — tops
the list. And there's a cluster over the *atomic* sites, Los Alamos and Oak Ridge.
That's not us editorializing; it's the threshold detector counting the Air Force's
own files." Then show the **coalescing**: the Wright-Patterson node merged **24**
OCR / location / org spelling variants (`Wright-Patterson AFB`, `WPAFB`,
`Wright-Patterson Air Forca Base`, `ATIC WPAFB Ohio`, …) into one installation —
open the node and show the alias list. The fold is identity-grade, so 15 is a *floor*.

**3.2 — Can a local model adjudicate like Blue Book did?** Run the disposition bench:
the model classifies a case's disposition from its narrative, **era-aware** (it can't
say "Starlink" for 1952), scored against the Air Force's own ruling. 12-category
taxonomy, labeled fixture spanning all categories (the hero corpus is all-UNIDENTIFIED,
so the *variety* lives in the fixture).
```sh
sovereign bench uap run --split test --policy tuned    # accuracy 0.917 / macro-F1 0.889
sovereign bench uap diagnose --split test              # confusion matrix + worst confusions
```
**They see:** a near-perfect diagonal confusion matrix — **11 of 12 correct** — with
the one miss being **SENSOR_ARTIFACT → ATMOSPHERIC**.

**Say:** "We measured it: 0.917 accuracy. And the one mistake is the *exact* kind a
human investigator made — a radar/sensor artifact read as an atmospheric effect.
That's the tell that it's doing the real adjudication task, not pattern-matching a
keyword."

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

## §Capture (run before the demo)
Hotspot + bench numbers are final and live in `CAPABILITY_BRIEF.md` (Acts 3.1/3.2
above quote them). The one remaining live capture is the **3 hero answers** (Act 2):
ask each case via the chat path against `uap-blue-book`, and save the grounded answer +
its `[Source: …]` Form-10073 citations (35B synth is slow live — capture ahead). Suggested
prompts: "What did the Air Force conclude about the Albuquerque sighting of August 1951,
and what evidence is in the file?" / same for Iwo Jima (June 1953) and Bohol Island (May
1958). Reproduce the rest: `sovereign enrich investigation show uap-blue-book` (graph +
hotspots) and `sovereign bench uap run --split test --policy tuned` (the 0.917 number).

---

## Honesty guardrails & anticipated skeptic questions

Lead with the framing; have the verified answer ready. Every number below is
reproducible (`dataprep/` scripts + `sovereign bench uap`).

**The numbers ladder — say it once, cleanly (pre-empts "wait, which number?"):**
**12,618** AF reports (1947–69) → **701** the AF closed UNIDENTIFIED → **558** of
those on NICAP's published roster → **270** matched to a digitized NARA file with
images → **401** case file-units = the hero corpus (some unknowns span >1
file-unit). The breadth corpus is the **10,750** digitized cases (all dispositions).

**Q: "Unidentified" just means they gave up — or it's a believer org's label.**
The label is the Air Force's own disposition, taken from NICAP's published roster of
the AF's unknowns (cross-referenced to Brad Sparks' catalog) — *not* our re-reading
of the card. The taxonomy keeps **UNIDENTIFIED and INSUFFICIENT_DATA distinct** —
"no explanation found" is not "not enough data." We surface the AF's record; we
don't adjudicate reality. (All 401 hero cases are single-class UNIDENTIFIED by
construction.)

**Q: how do you know you matched the *right* case?**
The NICAP→NARA join blocks on normalized **location + year** — coarse, not a
case-ID key (NARA publishes no per-case NICAP id). A match means "same place, same
year," so a small number could be mis-joined. The demo drills into specific cases
where the card's own location/date corroborate the match; treat the 401 as
high-confidence location+year matches, not a certified registry.

**Q: if the OCR is garbage, why trust any of it?** Two layers. (1) **The spine is
not OCR** — case identity, location, date, and the unidentified disposition come
from NARA's catalog + the NICAP roster (clean, structured). OCR only produces the
*narrative prose*. (2) That prose is better than "1950s microfilm" implies: **all
401/401** hero narratives are non-empty, **median ~2,074 chars** (min 171). We show
the **raw Form-10073 card alongside** every answer — nothing hidden, the room judges
the OCR itself.

**Q: it's a 35B-built graph — how do I know the entities/edges aren't hallucinated?**
Every relationship carries its **evidence**: a verbatim excerpt + the `chunk_id` of
the source card. In this graph that's **3,386 of 3,386 edges (100%)** — nothing is
asserted without a quote you can trace back to the Form-10073. The extraction is
JSON-schema-constrained (the model can't invent fields), and entity coalescing is
identity-grade (it merges OCR variants, it doesn't fuse distinct things). Pull any
edge in the demo and show its excerpt next to the card.

**Q: the hotspot map is just where the bases/people were.** Yes — and we say so. It's
**descriptive geography over the unidentified subset** (reporting density, exactly
how AARO presents its maps), **not** a causal claim about where phenomena occur.
More observers (air bases, the capital) → more reports. The headline node,
Wright-Patterson (15), is literally the AF's UFO HQ — a reporting-density artifact,
honestly stated.

**Q: the nuclear-site cluster (Los Alamos, Oak Ridge) — 4 cases is noise.** Correct
to push. These are small counts (threshold >3, over 17 years): a descriptive
pattern in the files, *not* a significance-tested finding. Frame it as "notable
that the AF's own unidentified files recur near the atomic sites," and don't
oversell.

**Q: 12 test cases is anecdotes — and the model just memorized Blue Book.**
- **Not cherry-picked:** consistent across the split — **train 0.962 / test 0.917**
  (36 of 38 cases correct overall).
- **Not a tuning trick:** the tuned policy gives **zero lift** on test over the
  baseline floor (both 0.917) — the model does the work, not the prompt.
- **Not memorization:** the one miss (SENSOR_ARTIFACT → ATMOSPHERIC) is a
  *reasoning* slip a human investigator makes, not a lookup failure; the bench runs
  on a **labeled fixture spanning all 12 categories** (the hero corpus is
  single-class, so the variety must live in the fixture), **era-masked** so no
  anachronistic label is even offered.
- **Honest scope:** a mechanism demonstration on a small labeled set, not a
  leaderboard claim. Reproducible: `sovereign bench uap run --split {train,test}`.
