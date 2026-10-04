# Ward CRM gold — labeling protocol v1 (2026-10-03)

Drafted by a different model family than the extractor (operator 2026-10-03: the
extractor's own model would draft toward what it already finds), reviewed by the
operator. Drafters are blind: they never read pipeline output (`~/.svrnmesh/indexes`,
`~/.svrnmesh/enrichment`). The ontology is `recipe.toml` beside this file.

## Unit and scope

One customer folder at a time, every file in
`~/.svrnmesh/bench-corpora/enron-ward/sample/<folder>/` (RFC-822, one message per
file). Deals span messages, so a folder is labelled as a whole.

Label what each message's author states. Quoted history (`-----Original Message-----`,
`>` lines) and forwarded blocks are labelled once, at the message in this folder that
carries them first by Date, with `"quoted": true` when the statement is not the
carrier's own.

## Records

- **person** — anyone in From/To/Cc or named in a body as acting for a party. `name`
  (fullest form seen), `emails` (every address seen for them), `company` (company id),
  `internal` (true for Enron staff).
- **company** — `name`, `domains` (address domains seen; never freemail: aol, hotmail,
  yahoo, msn, earthlink, worldnet, juno, compuserve). Enron and its affiliates are one
  company `enron` unless an affiliate is itself a party to a deal.
- **deal** — one prospective or actual transaction with ONE counterparty: `id`
  (`<folder>-d<n>`), `counterparty` (company id), `description` (product, delivery point,
  term — "5-yr fixed-price gas at PG&E Citygate"), `amount_usd` and `price` only when
  stated, `kind` (`transaction` or `master_agreement` for ISDA/GISB/EEI/enabling
  agreements with no specific trade), `messages` (file names).
- **stage_update** (claim on a deal) — a message stating or clearly implying where a deal
  stands. `deal`, `stage`, `quote` (verbatim, at most 200 characters, from that file),
  `file`, `date` (the file's Date header). Stages:
  `lead` inquiry, RFP or interest received; `proposal` indication, price or offer sent;
  `negotiating` terms, credit or contract drafts exchanged; `won` confirmed, executed or
  awarded to Enron; `lost` declined, awarded elsewhere or withdrawn.
- **commitment** (claim on a person) — someone promises to do something ("I will send the
  confirm today"). `person`, `what` (short), `due` (as stated, else null), `quote`,
  `file`, `date`.

Mark any label you are unsure of `"uncertain": "<why>"`; the operator reviews those
first. Never invent a field the text does not support; null is a label.

## Output

`~/.svrnmesh/bench-corpora/enron-ward/gold/<folder>.json`:

```json
{"folder": "...", "spec": "GOLD_SPEC v1", "files_read": ["1.", "2."],
 "files_with_nothing": ["7."],
 "people": [{"id": "p1", "name": "", "emails": [], "company": "c1", "internal": false}],
 "companies": [{"id": "c1", "name": "", "domains": []}],
 "deals": [{"id": "<folder>-d1", "counterparty": "c1", "description": "", "amount_usd": null,
            "price": null, "kind": "transaction", "messages": []}],
 "stage_updates": [{"deal": "<folder>-d1", "stage": "proposal", "quote": "", "file": "", "date": ""}],
 "commitments": [{"person": "p1", "what": "", "due": null, "quote": "", "file": "", "date": ""}]}
```

Ids are local to a folder; scoring matches people by email, companies by domain or
name, deals by counterparty plus description.
