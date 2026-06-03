# pass.toml — predicate vocabulary

Every key is optional. Present keys form an AND'd conjunction; the
scorer returns every reason that failed (not just the first).

Bool/integer/string keys take their natural TOML form. List keys are
TOML arrays of strings. Schema is enforced strict — a typo in a key
name fails fixture loading instead of being silently ignored.

## Decision axis (judiciousness)

| Key                       | Type      | Meaning                                                                                       |
|---------------------------|-----------|-----------------------------------------------------------------------------------------------|
| `should_call_search`      | bool      | Did the model invoke `search` (or `web_search`) at all?                                       |
| `forbidden_tools`         | [str]     | Tools the model must never call in this fixture.                                              |
| `expected_first_tool`     | str       | The first tool the model invokes must be this one. No constraint if absent.                   |
| `max_search_calls`        | int       | Cap on search invocations across the whole conversation. Catches loop bugs.                   |

## Query shape (anti-verbatim, anti-leak)

These are evaluated against the **first** search call's `query` arg,
case-insensitively.

| Key                              | Type     | Meaning                                                                              |
|----------------------------------|----------|--------------------------------------------------------------------------------------|
| `expected_query_contains`        | [str]    | Every listed substring must appear in the query.                                     |
| `expected_query_not_contains`    | [str]    | Every listed substring must NOT appear. Used to guard verbatim echoing of the prompt.|
| `expected_query_max_tokens`      | int      | Whitespace-token count cap on the query. Discourages paragraph-as-query.             |

## Result handling

When `should_call_search = true`, these gate the final synthesis turn.

| Key                                    | Type      | Meaning                                                                                                      |
|----------------------------------------|-----------|--------------------------------------------------------------------------------------------------------------|
| `must_cite_url_from_mock`              | int       | Minimum number of distinct mock-response URLs that must appear in the model's final message.                 |
| `must_not_cite_url_outside_mock`       | bool      | If true, any http(s) URL in the final message that isn't in the mock response is a failure (fabricated cite).|
| `contradiction_phrases`                | [str]     | Every listed substring (case-insensitive) must appear in the final message. For "two results disagree" fixtures.|
| `zero_results_must_be_acknowledged`    | bool      | When the mock returns 0 results, the final message must contain an empty/zero/no-results phrasing.           |

## Refusal path

| Key                          | Type   | Meaning                                                                                                              |
|------------------------------|--------|----------------------------------------------------------------------------------------------------------------------|
| `must_decline_gracefully`    | bool   | The model must NOT search and must produce a recognisable decline ("can't", "unable", "not configured", "no api key").|

## Final-message content

Pair these with skip-search fixtures (where the answer must come from
context or training) to catch the "skipped search but then
hallucinated" failure mode.

| Key                              | Type     | Meaning                                                                                |
|----------------------------------|----------|----------------------------------------------------------------------------------------|
| `final_message_contains`         | [str]    | Every listed substring (case-insensitive) must appear in the final assistant message.  |
| `final_message_not_contains`     | [str]    | None of the listed substrings (case-insensitive) may appear. Catches confident-wrong.  |

## Example

```toml
# fixtures/01_should_search_temporal_news/pass.toml

should_call_search = true
expected_first_tool = "search"
max_search_calls = 1

expected_query_contains = ["spacex", "starship"]
expected_query_not_contains = ["what happened with"]
expected_query_max_tokens = 8

must_cite_url_from_mock = 2
must_not_cite_url_outside_mock = true
```

## Rationale for the vocabulary

- **Why first-search-only for query-shape checks?** Multi-search
  fixtures usually involve refinement; the first query is where
  judiciousness shows. Later queries can be more specialised. Future
  vocabulary may add `expected_query_each_max_tokens` if a per-call
  cap becomes useful.
- **Why case-insensitive query matching?** The model's casing is
  non-deterministic across replays; fixture stability across model
  swaps matters more than capturing casing fidelity.
- **Why no LLM-as-judge in Phase 1?** Phase 1 pins structural
  invariants only. Synthesis-quality scoring lands in Phase 3 with a
  separate axis (1-5 scale, deterministic threshold) so it doesn't
  pollute the binary pass/fail of the structural checks.

## Adding a new key

1. Add the field to `Predicate` in `sovereign/crates/sovereign-cli/src/search_gym_cmd/predicate.rs`.
2. Document it here.
3. Add a scoring arm to `score::score()` in `score.rs`.
4. Pin behaviour with a unit test in `score.rs::tests`.
5. Use it in at least one fixture; failing fixtures don't catch
   regressions.
