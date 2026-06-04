# Capability Brief — UAP / Project Blue Book

The numbers behind the demo. Everything here traces to a public-domain source, a
committed script, or a committed bench — no proprietary data, nothing that left the
machine. `[FILL POST-ENRICH]` marks figures finalized once the hero enrichment
(35B over the 401 unidentified cases) completes.

## The archive (real, public, local)

| Metric | Value | Source |
|---|---|---|
| Digitized Blue Book cases ingested | **10,750** | NARA AWS Open Data, RG 341 (per-case fileUnits) |
| Scanned page images available | **126,428** | NARA-hosted public JPGs |
| Date span | **1947 – 1966** | NARA `logicalDate` |
| Air Force "UNIDENTIFIED" cases (the 701) | **558** catalogued | NICAP published list + Brad Sparks catalog |
| Unidentified cases matched to a digitized file w/ images (the **hero set**) | **401** (270 distinct NICAP) | location+year join, NICAP→NARA |
| Hero corpus chunks (OCR'd narratives) | **710** | PaddleOCR over Form-10073 cards |

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

## The evidence graph (hero set) `[FILL POST-ENRICH]`

| Metric | Value |
|---|---|
| Entities extracted (8 types) | `[FILL]` |
| Relationships (7 types) | `[FILL]` |
| Sighting-hotspot findings | `[FILL]` (installations w/ >3 nearby unidentified sightings) |
| Installations coalesced across OCR variants | `[FILL]` (e.g. Wright-Patterson / WPAFB / OCR noise → 1 node) |

## The disposition bench (measured, era-aware) `[FILL POST-ENRICH — real corpus]`

- Task: classify a case's disposition from its narrative, scored against the Air
  Force's own ruling. 12-category taxonomy; **date-conditioned era mask** (no
  "Starlink" for a 1952 case; the confusion matrix is read against era-possible labels).
- Synthetic-fixture baseline (mechanism proof): **accuracy 0.917 / macro-F1 0.889**,
  one real confusion (SENSOR_ARTIFACT→ATMOSPHERIC).
- Real-corpus numbers: `[FILL]` via `sovereign bench uap run|diagnose`.

## Honesty notes

- "UNIDENTIFIED" = the Air Force found no conventional explanation in that file. Not a
  claim about extraterrestrials.
- ~10,750 of 12,618 cases digitized; 401 of the 701 unknowns have a digitized
  image-backed file (rest are pre-1952 Sign/Grudge-era or undigitized).
- Bench is single-corpus, small held-out split — real but not a universal claim.
