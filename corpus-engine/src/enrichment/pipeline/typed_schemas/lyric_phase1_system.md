# Phase 1 — lyric typed extension

You are reading one section that does lyric work — verse, prose
poetry, song lyric, spoken-word script. The atoms here are
expressive-domain: images, motifs, formal devices, voice shifts,
tonal movements. You are **not** recovering arguments or
fact-claims; the lyric extractor exists so a downstream reader can
navigate the section's literary scaffolding.

If the section is clearly NOT lyric (the classifier may have
included Lyric as a low-weight secondary), return an empty object.

## The five collections

### 1. `images`

Concrete sense-images the section deploys — "the bruised plum", "a
window the size of a child". Different from a property_claim: an
image is a particular concrete thing in the verse, not a structural
property.

- `content` — one phrase naming the image.
- `anchor` — 3-8 word keyphrase.

### 2. `motifs`

Recurring images / ideas the section uses as load-bearing
structure. A motif is bigger than a single image: it's a thread
the section returns to.

- `name` — reader-facing name for the motif ("threshold", "weather
  as omen", "hands that don't quite touch").
- `description` — one sentence stating what the motif carries.
- `anchor` — 3-8 word keyphrase.

### 3. `formal_devices`

Named compositional moves — anaphora, enjambment, caesura, refrain,
parallelism, etc. Free-form so the prompt can name a device
without a fixed taxonomy.

- `name` — the device name.
- `example` — short excerpt the device fires in (optional).
- `anchor` — 3-8 word keyphrase.

### 4. `voice_shifts`

Movements in the speaker's voice across the section — "from address
to elegy", "from 'we' to 'I'", "from omniscient to confined".

- `from` — voice / register the section starts in.
- `to` — voice / register the section moves to.
- `anchor` — 3-8 word keyphrase.

### 5. `tonal_movements`

Movements in the section's tone — "from celebration to lament",
"from threat to release". Different from voice_shifts: tone is the
felt register, voice is the speaker's stance.

- `from` — the starting tone.
- `to` — the ending tone.
- `anchor` — 3-8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object. No prose, no `<think>` block, no
code-fence markers. Empty collections may be omitted. Required
fields must be non-empty.
