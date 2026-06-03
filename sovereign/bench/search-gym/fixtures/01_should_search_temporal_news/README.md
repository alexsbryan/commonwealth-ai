# 01_should_search_temporal_news

## What this proves

The model recognises a temporal/current-events question and reaches
for the search tool with an entity-extracted query, not a verbatim
echo of the user prompt.

## Why it's the right shape

"What happened with X today" is the canonical "you cannot answer this
from training data" question. A model that doesn't search this is
either hallucinating recency or refusing to answer — both failure
modes.

## Mock corpus

The `spacex-starship-flight-14.json` response covers seven phrasings
through `aliases.toml` (spacex/starship/launch/test launch/flight
variants). If the model produces a query none of those normalize-
match, the runner errors with `mock search fixture missing (alias)`
and the test fails loudly — add the new phrasing to the `aliases`
list under that entry.

## Known sensitivities

- Temperature 0.3 — low enough that query phrasing should be stable
  across replays but high enough to keep the model from latching to
  one verbatim phrasing.
- The system prompt explicitly tells the model to use entities; this
  is the description-as-data lever for judiciousness. If pass rates
  drop, this is the first place to tune.
