# Enron Email Corpus — recipes

Two recipes against the same CMU snapshot:

- `enron-sample` — all 150 mailboxes (~500k messages).
- `enron-sample-onemailbox` — Kenneth Lay's mailbox alone (~6k
  messages). Use this for reconciliation-policy tuning before
  committing to the full corpus.

Both drive the architecture-over-Enron substrate end-to-end:

```
local_file (maildir/)
  → email_rfc5322  (RFC-5322 + MIME, attachment dispatch)
      → described_asset  (asset store + parsed-form caches)
          → xlsx / docx / opaque sub-extractors
              → ExtractedDoc per attachment
  → boilerplate filter  (signature, quoted-reply, disclaimer strip)
  → paragraph chunker
  → conversation_atlas enrichment
      → entity extraction (LLM + GLiNER)
          → multi-origin reconciliation
              → ReconciledEntity + reconciliation_oplog.jsonl
```

## Operator setup

The CMU tarball is one-shot and large (423 MB). The acquirer
expects it already extracted at `~/.svrnmesh/corpora-staging/enron/maildir/`:

```sh
mkdir -p ~/.svrnmesh/corpora-staging/enron
cd ~/.svrnmesh/corpora-staging/enron
curl -L -O https://www.cs.cmu.edu/~enron/enron_mail_20150507.tar.gz

# Verify before extracting (regenerate this hash from the canonical
# CMU mirror on first download; commit the value into _provenance.json
# so re-extracts are bit-checkable).
shasum -a 256 enron_mail_20150507.tar.gz \
  | tee _provenance.json.partial

tar xzf enron_mail_20150507.tar.gz
# produces ./maildir/{user}/{folder}/...

# Optional: trim to a single mailbox for the fast-iteration recipe.
ls maildir/lay-k/   # confirms the Lay mailbox is in the extract
```

## Install + ingest

```sh
# Fast iteration — Lay mailbox only.
sovereign corpus install enron-sample-onemailbox

# Full corpus — only after the single-mailbox flow tunes cleanly.
sovereign corpus install enron-sample
```

## Bench

After enrichment lands:

```sh
# Floor — every surface form its own atom; pre-reconciliation
# baseline that tuning must beat.
sovereign bench enron run \
  --corpus enron-sample-onemailbox \
  --split train \
  --policy pre_reconciliation

# Tuned — reconciliation policy applied, B³ + pairwise-F1 computed.
sovereign bench enron run \
  --corpus enron-sample-onemailbox \
  --split train
```

Results land in `sovereign/bench/enron/baselines/enron-entity-resolution/`.

## Privacy posture

Enron is public record (FERC released it during the 2002 investigation),
but `mesh_sharing = false` anyway — these recipes are the substrate's
forcing function, not something to gossip across peers. The public-
release plan (recipe publish + holdout unseal + ground-truth curation)
is intentionally out of scope of the architecture-over-Enron push
and lives in a separate follow-up plan.
