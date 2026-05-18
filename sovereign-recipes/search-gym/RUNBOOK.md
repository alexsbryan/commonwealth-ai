# search-gym runbook

A correctness harness for the web-search-during-inference flow. Replays
fixtures against the running daemon, mocks the search backend with
on-disk fixtures, and scores the model's tool-call judiciousness and
synthesis quality.

## What this proves

Three claims:

1. **Judicious invocation.** The model invokes `search` for questions
   that genuinely need current/external information and not otherwise.
   Reflexive search on every prompt fails; never searching when the
   answer is "yesterday's stock close" also fails.
2. **Faithful synthesis.** Claims drawn from search results are
   traceable to URLs in the result set. No fabricated cites.
3. **Deterministic.** CI never spends Tavily quota. The mock backend
   resolves queries against on-disk fixture files; a missing fixture
   is a loud error, never silent fall-through.

## Running

```
sovereign daemon run &                       # in another shell / tmux
sovereign search-gym run                     # all fixtures, 10 replays each
sovereign search-gym run --fixture 01_should_search_temporal_news --replays 3
sovereign search-gym run --json > run.json   # for diffing across model changes
```

The gym refuses to start if the daemon isn't reachable at
`http://localhost:9741` (override with `--base-url`).

## Fixture layout

```
fixtures/<NN_slug>/
  input.json    — full ChatCompletionRequest. Includes the model's
                  tool list (tools=[search,...]) and the user prompt.
                  Stream is forced to false by the runner.
  pass.toml     — predicate (see PASS_SCHEMA.md).
  README.md     — what this fixture proves, in prose. Not parsed.
```

## Mock-corpus layout

```
mock-corpus/
  aliases.toml              ← human-readable filename + alias index
  <fixture-name>.json       ← named per alias entry
  <sha256_of_normalized_query>.json   ← optional hash-keyed fallback
```

Lookup is two-tier:

1. **Alias hit.** The runner normalizes the query (lowercase, trim,
   collapse whitespace) and looks for a matching alias in
   `aliases.toml`. If found, loads the named file. This is the
   default authoring pattern — multiple model phrasings → one
   response file.
2. **Hash fallback.** If no alias matches, the runner looks for
   `<sha256(normalized_query)>.json`. This is the rarely-used path
   for one-off responses or recordings imported wholesale from
   `sovereign bench search-tavily` (Phase 4 — not yet landed).

A missing fixture in either mode is a **loud error** — the runner
echoes the offending query and the path it tried.

### aliases.toml shape

```toml
[[entry]]
file    = "spacex-starship-flight-14.json"
aliases = [
  "spacex starship test launch",
  "latest spacex starship test launch",
  "spacex starship flight",
]

[[entry]]
file    = "nvda-stock-quote.json"
aliases = [
  "nvda stock price",
  "nvidia stock price",
]
```

Schema is strict (`deny_unknown_fields`); a typo in `file` or
`aliases` fails the index load instead of being silently ignored.

### Response file shape

```json
{
  "query": "spacex starship flight 12",
  "results": [
    {
      "title": "...",
      "url": "https://example.com/...",
      "snippet": "..."
    }
  ]
}
```

The `query` field is for human readability (so `grep` over the
corpus is useful); it isn't used for matching. Set it to the
canonical phrasing for the cluster.

### Hash computation (when you do need it)

```
$ echo -n "spacex starship flight 12" | python3 -c \
    "import sys, hashlib; print(hashlib.sha256(sys.stdin.read().encode()).hexdigest())"
```

Same normalization rule as `mock_fixture_hash()` in
`sovereign-tools/src/web/search.rs`.

## When a fixture fails

The summary table prints the first few failure reasons per fixture.
Re-run with `--json` to capture the full transcript, including every
tool_call the model emitted and the cited URLs from the final message.

If the runner errors with `mock search fixture missing`, the model
asked for a query you haven't recorded. Either:

- The fixture's prompt is too open-ended (model is reformulating in
  many ways). Tighten the prompt and re-record.
- The query is legitimate but uncovered. Record the response and add
  it to mock-corpus/. Use `sovereign bench search-tavily --record`
  (Phase 4 — not yet landed) to fetch a real response if you have
  the budget. For now, hand-author.

## Budget invariant

The mock backend still calls `decrement_budget` on the `SearchTool`,
so the gym's runs are visible in the monthly counter. This is
deliberate — it pins the invariant that no provider, including Mock,
can bypass budget tracking. Future work (Phase 4) introduces real
Tavily calls with hard budget gating; the test that proves the
budget can't be sidestepped lives at `web::search::tests::mock_provider_*`.

## Phase status

- [x] Phase 1 — scaffolding, mock backend, runner skeleton, 5 seed fixtures
- [ ] Phase 2 — judiciousness fixture expansion (→15) + baseline run
- [ ] Phase 3 — result-handling fixtures (→25) + LLM-as-judge synthesis
- [ ] Phase 4 — `sovereign bench search-tavily` + budget tracker + drift report
- [ ] Phase 5a/b/c — server-side gate, refusal path, tracing
- [ ] Phase 6 — SYSTEM_OVERVIEW + WEB_SEARCH.md
