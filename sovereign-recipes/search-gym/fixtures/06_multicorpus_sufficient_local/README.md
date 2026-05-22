# 06_multicorpus_sufficient_local

## Archetype: local is sufficient

The user asks about a stable scientific concept. Both `knowledge`
and `search` tools are available. A model that reflexively reaches
for `search` fails the production cost-awareness test — local
knowledge (or even the model's own training) is enough here.

## What this proves

The model distinguishes "stable concept I can answer" from "current
information I need to search for". When given three tools, the
model should pick the cheapest one that answers the question. For
photosynthesis, that's "no tool at all" or "knowledge if I want
citations" — never web search.

## Mock corpus

If the model decides to call `knowledge`, the runner serves an
on-topic article excerpt from `mock-corpus/knowledge/`. If it
calls `files`, files/ returns nothing (the user has no notes about
photosynthesis). The fixture passes as long as the model doesn't
call `search`.

## Why these predicates

- `should_call_search = false` — primary axis
- `forbidden_tools = ["search", "web_search"]` — explicit ban
- `final_message_satisfies` — content correctness (judge), so a
  model that "skips search but produces gibberish" still fails

## Known sensitivities

If you see this fail with the model calling `search`, the tool
description's "prefer local tools when they can answer" line is
the lever to tighten.
