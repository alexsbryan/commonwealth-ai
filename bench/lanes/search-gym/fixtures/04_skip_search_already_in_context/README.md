# 04_skip_search_already_in_context

## What this proves

The model uses its conversation context before reaching for external
tools. The user supplied "96 GB of unified memory" two turns up;
re-searching is a context-attention failure.

## Why this specific framing

A naive read of "current/recent" might tempt a model to search for
"AMD Radeon 8060S unified memory". The right answer is right above —
the user stated the figure. This pins context-attention as a
prerequisite for tool-call judiciousness.

## Mock corpus

None. Search shouldn't happen.

## Known sensitivities

- If the model returns a different memory figure than 96 GB, this
  fixture still passes (the predicate only checks `should_call_search
  = false`). Phase 2 may add a `final_message_contains` predicate
  for content correctness.
