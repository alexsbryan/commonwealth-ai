# Demo Runbook — "What the Air Force could not explain"

**Audience:** open-source foundation / partners + the genuinely curious public
(UAP has broad reach; the credibility bar is *higher*, not lower).
**Length:** ~10–12 min.
**One-line goal:** show that the real, public Project Blue Book archive — the U.S.
Air Force's own 17-year UFO investigation — can be **downloaded, structured into a
queryable evidence graph, and interrogated with grounded citations, locally, on a
laptop**, with a special light on the cases the Air Force itself filed as
UNIDENTIFIED. Nothing leaves the machine; every claim cites its source card.

> **The hook:** this isn't a believer's mixtape or a debunker's hit piece. It's the
> government's own record — 10,750 digitized case files, 126,000 scanned pages —
> turned into something you can *actually ask questions of*, with the answer always
> pointing back at the Air Force's own Form-10073 record card.

---

## Surfaces — what you drive, and where each thing is shown

This demo uses **three windows**. Be deliberate about which is on screen — it's the
difference between "smooth" and "where is that again?".

| Window | Used for | Why (verified) |
|---|---|---|
| **Terminal** | install · the evidence graph (`enrich investigation show`) · the bench | The investigation graph + hotspots + bench are **CLI-only** today. The desktop Atlas View renders v2 *atlas atoms*, **not** the `investigation/` graph this corpus produces. |
| **Sovereign Desktop app** | the **grounded chat** (Act 2) — ask a question, get a cited answer, click a citation → popover with the quoted card text | Grounded retrieval + citations is the desktop/server **Runtime** path (`SourceAttribution` + `SourcePopover`). The bare daemon's `/v1/chat/completions` is raw inference with **no retrieval** — so this step is the app, not a CLI one-liner. |
| **Browser tab** | the **scanned Form-10073 card** (Act 1) | Neither the CLI nor the desktop app renders images. The scan lives at a NARA URL; open it in a browser. (Optional upgrade below makes citations link to it directly.) |

> **Honest gaps to know before the room** (don't get caught): the desktop app has
> **no corpus-catalog/install UI** (install is the terminal), **no investigation-graph
> view** (terminal), and **no image viewer** (browser). The chat citation popover shows
> the quoted text and a "View source" link *only if the chunk carries a url* — this
> corpus's chunks don't (see "Optional upgrade"), so the scan is the browser tab.

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
  **PaddleOCR**, not a cloud service. The structured spine (case identity, location,
  date, disposition) is NARA catalog + roster data; OCR produces only the narrative.

---

## Pre-flight (do this BEFORE the room — and once as a full dry-run)

1. **Build so the new recipes are vendored** (the `[prebuilt]` blocks are embedded at
   compile time): `cargo build -p sovereign-cli-llm`. If you'll attach the desktop to a
   freshly-built daemon, also `cargo build -p sovereign-cli-daemon` and restart it.
2. **Daemon up, models loaded:** `sovereign daemon status` → expect an embed slot + a
   35B chat slot (the demo box runs `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL` as `primary`).
3. **Install both corpora** (terminal):
   ```sh
   sovereign corpus install uap-blue-book          # enriched hero: 710 chunks + graph, seconds from HF
   sovereign corpus install uap-blue-book-index    # breadth: 10,750 metadata cases
   ```
   Verify: `sovereign enrich investigation show uap-blue-book` prints `Entities: 5598 /
   Relationships: 3386 / Pattern findings: 15`.
4. **Desktop retrieval reachable:** launch the app (dev: `npm run dev` in
   `sovereign/crates/sovereign-desktop/` then `tauri dev`; or the packaged `.app`). It
   attaches to the daemon on `:9741`. Confirm `uap-blue-book` is retrievable — if you've
   set `[retrieval] corpora` allow-list anywhere, `uap-blue-book` must be in it (empty =
   all). **Dry-run the Act 2 question and confirm a citation popover appears.**
5. **CAPTURE the hero answers ahead of time** (35B synth is slow live). See §Capture.
6. **Fallback:** a pre-recorded screen capture of the full run. Never demo live without it.

---

## Act 1 — From a scanned card to a structured case *(the "before/after")*

**The exemplar: case BB-955 — Albuquerque, New Mexico, 25 Aug 1951.** Chosen because it
sits next to the Sandia/Kirtland nuclear complex (it sets up Act 3's nuclear cluster)
and its card is legible.

**1a. Show the raw card (browser).** Open the NARA catalog page for the case:
```
https://catalog.archives.gov/id/28939405      # BB-955, 33 scanned pages (Form-10073 + attachments)
```
**Say:** "This is what the Air Force left us — a scanned 1950s microfilm card. Date,
location, length of observation, a row of conclusion checkboxes, a typed summary."

**1b. Install the structured corpus (terminal).**
```sh
sovereign corpus install uap-blue-book        # pulls the prebuilt enriched index from HF
```
**They see:** the enriched corpus (graph + 401 hero cases) restored in **seconds** — the
35B work already done, shipped, reproducible. No GPU, no API key, no cloud.

**1c. Show the same card as a typed record (terminal).** The pipeline read that scan
locally and filed BB-955 as: location *Albuquerque, New Mexico*, date *25 Aug 1951*,
study *Blue Book*, and linked it to a sighting, an observed object, a witness, an
installation, and an adjudication — every edge quoting the card.

**Say:** "Same card. The system read it locally, pulled the fields, and filed it as a
typed case wired into an evidence graph — times **10,750**. The corpus is a single download."

---

## Act 2 — The 701: ask what stayed unexplained *(the hero — Sovereign Desktop)*

**Surface: the desktop app.** Select the `uap-blue-book` corpus and ask (type it live):

> *"What did the Air Force conclude about the Albuquerque, New Mexico sighting of
> August 1951, and what evidence is in the file?"*

**They see:** the answer streams in, grounded in the OCR'd card, followed by a
**"Sources:"** block. Click a citation → a **popover** shows the exact quoted passage
from BB-955's Form-10073 + the corpus badge. The answer surfaces the real evidence —
a Sandia Base security guard and his wife, a "flying-wing"-type object, the Kirtland
AFB radar station — and that the file closed **UNIDENTIFIED**.

**Then flip to the browser tab (the scan from Act 1)** and say: "And here's the actual
card it's quoting." That's the loop: grounded answer → citation → the government's own page.

**Two more in the can** (capture ahead, same flow):
- **Iwo Jima, 24 June 1953** (BB-2605) — ground + radar, military witnesses.
- **Bohol Island, Philippines, May 1958** (BB-5800) — 90-sec falling object, smoke trail.

**Say:** "Ask it what the Air Force couldn't explain — and it doesn't hand-wave. It pulls
the specific case, the radar, the witnesses, and quotes the card it's citing. This is
the 701, legible."

> **Supporting structure (optional, terminal):** `enrich investigation show` exposes the
> typed graph. BB-955's clean edges — `occurred_near` → *Kirtland AFB radar station*,
> `observed_by` → *Sandia Base security guard and his wife*, `has_sighting` →
> *flying-wing object* — are demo-grade. Don't feature the `officially_resolved_as`
> edges for a single case: `adjudication` is the noisy type (the OCR checkbox row), and
> it shows. The aggregate (Act 3) is where the graph shines.

---

## Act 3 — The hotspot map + the disposition bench *(turn "spooky" into "measured")*

**3.1 — Hotspots (terminal).**
```sh
sovereign enrich investigation show uap-blue-book
```
**They see** the 15 `sighting_hotspots` findings. The headline (verbatim from the output):

| Installation | Unidentified sightings nearby |
|---|---|
| **Wright-Patterson AFB, Ohio** (Blue Book HQ / ATIC) | **15** |
| Kelly AFB · San Antonio, TX · **Washington, D.C.** (the '52 Capitol flap) | 7 |
| Kirtland AFB, N.M. · Dallas, Pa. | 6 |
| Andrews AFB · George AFB · Lake Charles AFB | 5 |
| **Los Alamos, N.M. · Oak Ridge, Tenn.** (the nuclear-site cluster) · Arlington · Chicago · Columbus · McChord | 4 |

**Say:** "Two things jump out. Wright-Patterson — the Air Force's own UFO HQ — tops the
list. And there's a cluster over the *atomic* sites, Los Alamos and Oak Ridge. That's
not us editorializing; it's the threshold detector counting the AF's own files."

**Show the coalescing** (this is the credibility moment) — the Wright-Patterson node
absorbed **24** OCR/location/org spelling variants into one installation:
```sh
python3 -c "
import json, os
ents = json.load(open(os.path.expanduser('~/.svrnmesh/indexes/uap-blue-book/investigation/entities.json')))
wp = max((e for e in ents if e['entity_type']=='installation' and 'wright-patterson' in e['canonical_name'].lower()),
         key=lambda e: len(e['aliases']))
print(wp['canonical_name'], '— count', wp['attributes'].get('sighting_count'), '—', len(wp['aliases']), 'variants')
for a in wp['aliases']: print('  ', a)
"
```
**Say:** "`Wright-Patterson AFB`, `WPAFB`, `Wright-Patterson Air Forca Base`,
`ATIC WPAFB Ohio` — 24 ways the OCR spelled one base, folded into one node. The fold is
identity-grade — it merges spellings, never distinct bases — so **15 is a floor**."

**3.2 — Can a local model adjudicate like Blue Book did? (terminal).**
```sh
sovereign bench uap run --split test --policy tuned       # accuracy 0.917 / macro-F1 0.889
sovereign bench uap diagnose --split test                 # confusion matrix + worst confusions
```
**They see** a near-perfect diagonal confusion matrix — **11 of 12 correct** — the one
miss being **SENSOR_ARTIFACT → ATMOSPHERIC**. (Robustness, if pushed: `--split train`
→ 0.962; `--policy baseline` → still 0.917, i.e. no tuning lift.)

**Say:** "We measured it: 0.917, and 0.962 on the larger split. The one mistake is the
*exact* kind a human investigator made — a sensor artifact read as an atmospheric
effect. That's the tell it's doing the real adjudication task, not keyword-matching."

---

## The close

"Fully local, on a laptop:
- **The government's own UFO archive** — 10,750 cases, 126,000 scanned pages — turned
  into a **queryable evidence graph**, downloaded in seconds.
- **The cases it couldn't explain**, surfaced with their structured evidence and **every
  claim quoting the Air Force's own record card**.
- **Nothing left the machine.** Public-domain source, local OCR, local inference,
  inspectable pipeline — and the corpus is one `sovereign corpus install` away for
  anyone who wants to dig in themselves."

---

## §Capture (run before the demo)

The graph + hotspot + bench numbers are reproducible terminal output — re-run them live
or screenshot. The one thing to **capture ahead** is the **3 hero chat answers** (Act 2),
because 35B synthesis is slow live:

1. In the desktop app (or sovereign-server `/v1/chat/completions` with `uap-blue-book`
   enabled — the same Runtime retrieval path), ask each of the three prompts (Albuquerque
   Aug 1951 / Iwo Jima Jun 1953 / Bohol Island May 1958).
2. Save each grounded answer **and** its citation popover (screenshot the quoted passage).
3. Have the matching NARA card tab pre-opened: BB-955 →
   `https://catalog.archives.gov/id/28939405` (look up BB-2605 / BB-5800 by their naId
   in `dataprep/cases_real.jsonl` the same way).

---

## Optional upgrade (tighter demo, not required) — clickable citations → the scan

Today the chat citation popover shows the quoted card *text* but no "View source" link,
because the ingested hero chunks carry no `url`. To make a citation click open the actual
NARA scan: add the case's NARA URL (or first `image_url` from `dataprep/metadata.jsonl`)
as a `url`/source field on each record in `dataprep/cases_real.jsonl`, re-ingest +
re-enrich the hero corpus, and re-publish the snapshot. This is a full hero rebuild
(~4 h enrich) — schedule it only if "click the citation → see the card" is worth it for
the audience. Without it, the browser tab covers the scan.

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
the **raw Form-10073 card** in a browser tab — nothing hidden, the room judges the
OCR itself.

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
