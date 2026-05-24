**Atom-naming discipline (load-bearing for source recovery):**

1. **Prefer verbatim phrasings from the source excerpts above** when naming
   atoms. The cluster/theme summary is a paraphrase produced by the RAPTOR
   summariser — it strips distinctive vocabulary (e.g. "spread pricing"
   may show up only as "buying drugs cheap and billing payers more" in the
   summary). The source excerpts hold the verbatim phrasings the
   summariser dropped. When an excerpt names a mechanism, a position, a
   piece of evidence, or an opposition with a distinctive multi-word
   phrase, USE THAT EXACT PHRASE in the atom's name/label.

2. **Do NOT invent prose names.** Names like "PBM administrative fee
   expansion" or "buyer-seller matching" are paraphrase that lose the
   audit trail. Names like "spread pricing", "tragedy of the commons",
   "EUV monopoly", "$1.4B FTC PBM spread" preserve it — a downstream
   reader can grep them against the source. The reader must be able to
   recover what the source said from the atom name alone.

3. **Opposition labels are SHORT.** Two to four words per side. "markets
   vs regulation" — NOT "US hyperscaler dominance vs decentralized
   sovereign infrastructure". Long verbose labels fail to resolve to the
   source's named contrasts.

4. **Evidence labels lead with the distinctive token** — a dollar figure
   ("$1.4B FTC PBM spread"), a named study ("Ostrom 1990 commons"), a
   case name ("Pruitt-Igoe"), a percentage ("58% Micron net margin").
   If the excerpts contain a numeric or proper-noun anchor, that anchor
   becomes the label.

5. **The primary_entities list above carries vault-canonical names —
   prefer them as atom names when the entity is also a mechanism or a
   position the source argues with.** Example: if "Spread Pricing" is
   in primary_entities AND the source excerpts use that exact phrase,
   the mechanism atom's name is "spread pricing" — not a coinage that
   restates what spread pricing does.

This discipline matters because the bench's atom scorer matches by
name. An atom that captured the right argumentative move but renamed
it doesn't surface as a hit — it surfaces as a miss plus a
fabrication. The system's glassbox premise is that an operator can
trace every atom back to source words; paraphrased names break that
contract structurally.