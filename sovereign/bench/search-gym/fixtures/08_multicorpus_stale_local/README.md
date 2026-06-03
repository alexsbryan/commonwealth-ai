# 08_multicorpus_stale_local

## Archetype: local is stale

Local knowledge has an article that mentions a price/figure from
a previous snapshot. The user asks for "current" price. A model
that trusts the local figure quotes stale data as current —
quietly wrong is the worst failure mode.

## What this proves

Recency awareness in the face of plausible-looking local data.
The local hit contains a *real* price; the prompt asks for the
*current* one. The model has to recognise that any locally-stored
price is by definition not current, and reach for web search.

## Mock corpus

- `knowledge/nvidia.json` — encyclopedia-style article with a
  parenthetical "($142 as of January 2024)" — accurate at write
  time, stale at read time
- `web/nvda-stock-quote.json` — current quote (re-used from
  fixture 02's web corpus)

## Why this matters

Local corpora are CRUDable. Wikipedia gets edited; SEP articles
get revised; personal notes go stale. Any "stable knowledge"
corpus inherits the staleness of its sources. The model can't
assume local = correct just because local is local.

## Known sensitivities

This is one of the harder archetypes. Even strong models will
often confidently quote the stale figure. If failures cluster
here, the system prompt could explicitly call out that
"local prices and figures are historical, not current".
