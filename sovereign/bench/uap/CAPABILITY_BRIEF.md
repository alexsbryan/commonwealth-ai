# Capability Brief — UAP / Project Blue Book

The numbers behind the demo. Everything here traces to a public-domain source, a
committed script, or a committed bench — no proprietary data, nothing that left the
machine. Graph figures are from the completed hero enrichment (35B over the 401
unidentified cases) + the deterministic re-fold; the bench figures are a live-model
run, reproducible with the command shown.

## The archive (real, public, local)

| Metric | Value | Source |
|---|---|---|
| Digitized Blue Book cases ingested | **10,750** | NARA AWS Open Data, RG 341 (per-case fileUnits) |
| Scanned page images available | **126,428** | NARA-hosted public JPGs |
| Date span | **1947 – 1966** (Blue Book era; raw `logicalDate` carries rare pre-1947 outliers) | NARA `logicalDate` |
| Air Force "UNIDENTIFIED" total (the famous count) | **701** | USAF Blue Book final report |
| …published as a machine-readable roster | **558** | NICAP list + Brad Sparks catalog |
| …matched to a digitized NARA file w/ images (the **hero set**) | **270** distinct → **401** file-units | location+year join, NICAP→NARA |
| Hero corpus chunks (OCR'd narratives) | **710** | PaddleOCR over Form-10073 cards |

Numbers ladder, read top→bottom: 12,618 reports → 701 unknown → 558 rostered → 270
matched → 401 file-units. The **structured spine** (case identity, location, date,
the unidentified disposition) is NARA catalog + roster data — **not** OCR; OCR
produces only the narrative prose (all 401 non-empty, median ~2,074 chars).

**Access cost to a curious downloader:** one `sovereign corpus install uap-blue-book`
→ the **enriched** corpus (graph + cited narratives) restored from a HuggingFace
prebuilt snapshot in seconds. No GPU, no API key, no cloud inference. The full-catalog
metadata (`uap-blue-book-index`, all 10,750) is a second install for breadth.

## The pipeline (all local, all inspectable)

- **Acquire:** public NARA AWS bucket — no key, no email, partitioned to RG 341
  (a few hundred MB, not the 70 GB monolith). Per-case fileUnits → **no microfilm
  segmentation needed** (NARA already split the rolls into cases).
- **OCR:** **PaddleOCR** (ONNX, local) over the per-case page JPGs — the Air Force
  Form-10073 record card → date, location, length-of-observation, conclusion checkbox,
  brief summary. Noisy but faithful; raw card shown alongside in the demo.
- **Enrichment:** the recipe-declared investigation pipeline → a typed graph
  (case / sighting / observed_object / witness / installation / investigating_body /
  adjudication) with a sighting-hotspot threshold. 35B, local.
- **Distribution:** `corpus snapshot publish --upload <hf>` bundles chunks + atlas +
  the investigation graph; `[recipes.prebuilt]` makes the cold-start a single install.

## The evidence graph (hero set)

Built by the recipe-declared investigation pipeline (35B, local) over the 710
OCR'd hero chunks, then deterministically re-folded (`enrich investigation
recoalesce`) under the recipe's coalescing rules — no re-inference.

| Metric | Value |
|---|---|
| Entities (8 types: case / sighting / observed_object / witness / installation / investigating_body / adjudication / weather_context) | **5,598** |
| Relationships (7 types) | **3,386** |
| Sighting-hotspot findings | **15** (installations with >3 nearby unidentified sightings) |
| OCR / location / org variants folded into the Wright-Patterson node | **24 → 1** (count 15) |

Top hotspots (descriptive geography, à la AARO's maps): **Wright-Patterson 15**,
**Washington D.C. 7** (the 1952 Capitol radar-visual flap), San Antonio 7,
Kelly AFB 7, Kirtland 6, Lake Charles 5, George AFB 5 — plus a **nuclear-site
cluster** (Los Alamos, Oak Ridge). Coalescing is identity-grade (only OCR
suffix / qualifier regions fold; base tokens stay exact), so a base's true
count is ≥ what's shown — the numbers are conservative, never inflated.

## The disposition bench (measured, era-aware)

- Task: classify a case's disposition from its narrative, scored against the Air
  Force's own ruling. 12-category taxonomy; **date-conditioned era mask** (no
  "Starlink" for a 1952 case; the confusion matrix is read against era-possible labels).
- Against the **live local 35B**: **test 0.917 / macro-F1 0.889** (12 cases),
  **train 0.962 / 0.907** (26 cases) — **36 of 38 correct**, consistent across the
  split (not cherry-picked). One real confusion — **SENSOR_ARTIFACT → ATMOSPHERIC**
  (the night-sky misread a human investigator would make too). The `tuned` policy
  gives **zero lift** over the `baseline` floor on test (both 0.917) — the model is
  doing the work, not the prompt. Reproduce:
  `sovereign bench uap run --split {train,test} --policy {baseline,tuned}`.
- The bench runs on a **labeled fixture set** spanning all 12 categories, *not*
  the hero corpus: the 401 image-backed hero cases are all the Air Force's own
  UNIDENTIFIED (single-class by construction), so the disposition *variety* must
  live in the fixture. The hero corpus carries the graph + grounded retrieval;
  the fixture bench carries the "can a local model adjudicate like Blue Book did?"
  measurement. Both are real; neither is synthetic-data-for-the-graph.

## Honesty notes (see DEMO_RUNBOOK.md "anticipated skeptic questions" for the full Q&A)

- "UNIDENTIFIED" = the Air Force found no conventional explanation in that file — its
  own disposition (from the NICAP/Sparks roster of the AF unknowns), not our reading
  of the card, and not a claim about extraterrestrials. UNIDENTIFIED ≠ INSUFFICIENT_DATA
  (distinct categories).
- Coverage, reconciled: 10,750 of 12,618 cases digitized; of the 701 AF unknowns,
  558 are on NICAP's roster, 270 matched a digitized image-backed file → 401
  file-units (the rest are pre-1952 Sign/Grudge-era or undigitized).
- The NICAP→NARA match is **location + year** (coarse), not a case-ID key — a small
  number could be mis-joined; treat the 401 as high-confidence matches, not a registry.
- Hotspots are **descriptive geography** (reporting density over the unidentified
  subset, à la AARO), not a causal claim; small clusters (e.g. nuclear sites, n=4)
  are patterns in the files, not significance-tested findings.
- Bench is a small labeled fixture (38 cases) run live — a mechanism demonstration,
  not a universal claim; 36/38 correct, consistent across the train/test split.
