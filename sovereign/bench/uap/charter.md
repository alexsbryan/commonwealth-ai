# Charter — UAP / Project Blue Book demo corpus

**Corpus id:** `uap-blue-book`
**Who it's for:** researchers studying how the U.S. Air Force historically
adjudicated UAP ("UFO") sightings — Projects Sign / Grudge / Blue Book.

## The source (Level 1)

A local folder of **case-metadata records, one JSON object per line (JSONL)**.
It's already on disk — I'll give you the path. Each record has structured
fields plus a `narrative` field that prose-describes the case. Representative
record:

```
{"case_id":"BB-1953-0617","date":"1953-06-17","location":"Wright-Patterson AFB, Ohio",
 "shape":"DISC","disposition":"UNIDENTIFIED",
 "narrative":"At 02:10 ... a radar operator and two pilots tracked a silent disc ...
 ATIC investigators could not reconcile the radar return ... listed UNIDENTIFIED."}
```

The `narrative` is the main text to index and to extract from. `case_id` is the
stable handle. This is a **local folder + JSONL** source — not a web API.

## What I want out of it

1. A searchable corpus over the case narratives.
2. An **investigation enrichment** pass that pulls a typed graph out of the
   narratives, so I can ask structural questions. Specifically:

**Entities to extract** (from the narrative prose):
- **Case** — the archival case file (its `case_id`).
- **Sighting** — the observed event (when, time of day, duration, witness count,
  detection method: visual / radar / visual+radar / photo).
- **ObservedObject** — what was seen: shape (disc / cigar / light / triangle /
  sphere / formation / other), color, motion (hover / erratic / high-speed /
  steady / formation / descending), sound.
- **Witness** — role + reliability only (military / civilian / pilot / radar
  operator / law enforcement / scientist). Names are not present; don't invent them.
- **Installation** — the military facility involved (e.g. Wright-Patterson AFB),
  with branch and type.
- **InvestigatingBody** — who investigated (ATIC, OSI, project staff).
- **Adjudication** — the official ruling / disposition (e.g. ASTRONOMICAL,
  AIRCRAFT, BALLOON, UNIDENTIFIED) and its rationale.
- **WeatherContext** — atmospheric / astronomical conditions offered as an
  explanation (visibility, cloud cover, celestial body present like Venus,
  temperature inversion).

**Relationships** (typed edges between those entities):
- Case **has** Sighting
- Sighting **involves** ObservedObject
- Sighting **observed_by** Witness
- Sighting **occurred_near** Installation
- Sighting **under_conditions** WeatherContext
- Case **investigated_by** InvestigatingBody
- Case **officially_resolved_as** Adjudication

**One pattern detector** — a **threshold**: surface installations that have more
than 3 sightings occurring near them (descriptive geography — which bases show up
most). Do **not** add circular-flow or role-overlap detectors; on this data they
manufacture false patterns.

## Boundaries already settled

- Level 1 is the JSONL metadata only. Scanned PDFs are a separate, later recipe.
- Witnesses are modeled as role + reliability, never identity (the records are
  redacted of names).
- Disposition labels follow the Blue Book / AARO unified taxonomy
  (ASTRONOMICAL, AIRCRAFT, BALLOON, SATELLITE, UAS_DRONE, BIRD, ATMOSPHERIC,
  SENSOR_ARTIFACT, HOAX, OTHER_IDENTIFIED, INSUFFICIENT_DATA, UNIDENTIFIED).
