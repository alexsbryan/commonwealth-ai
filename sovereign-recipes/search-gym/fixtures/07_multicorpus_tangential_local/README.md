# 07_multicorpus_tangential_local

## Archetype: local is tangential (looks helpful, isn't)

Knowledge corpus has a general SpaceX article — founding,
reusable-rocket philosophy, vehicle generations. It does NOT have
data on the most recent specific test flight. A naive model sees
the local hit and synthesises "yes/no" from general context,
hallucinating recency. The correct behavior: recognise the local
hit doesn't answer the specific question and reach for `search`.

## What this proves

Local-corpus hits can be topically relevant but **insufficient**.
Discrimination between "I have something on this topic" and "I
have the answer to this question" is the load-bearing skill.
Failing this fixture in production means the model confidently
gives stale or fabricated answers about current events whenever
the topic happens to be in Wikipedia.

## Mock corpus

- `knowledge/spacex.json` — general SpaceX history, founding date,
  vehicle generations, no specific recent-flight info
- `web/spacex-starship-flight-14.json` — current article about the
  actual recent test flight (re-used from fixture 01)

## Predicates

- `should_call_search = true` — primary axis (eventual web search)
- `max_search_calls = 2` — allow one retry, no looping
- `must_cite_url_from_mock` — final synthesis must come from web result
- `must_not_cite_url_outside_mock` — no URL fabrication
- `final_message_satisfies` — judge confirms answer uses current
  information, not pre-2002 general SpaceX context

## Known sensitivities

A weak model may answer from the knowledge article and claim
"yes, SpaceX has caught boosters before" without specifically
verifying the most recent flight. The judge assertion is worded
to catch this — the response must address "the most recent
test flight" specifically.
