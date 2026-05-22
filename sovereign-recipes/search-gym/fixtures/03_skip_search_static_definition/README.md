# 03_skip_search_static_definition

## What this proves

The model recognises a stable-concept question and doesn't reach
reflexively for search. Pin the negative axis — over-eager search is
a budget-burner.

## Why a monad

"Monad in functional programming" is canonically well-covered by
training data across every model class. If a model searches this, it
either doesn't trust its training or hasn't internalised the cost
signal in the tool description.

## Mock corpus

None. This fixture asserts no search happens. If the model does
search, the runner will hit a `mock search fixture missing` error,
which the scorer surfaces as a `runner_error` — still a fail, just
with a different failure shape.

## Known sensitivities

If you see this fixture fail with the model searching, the cost-
awareness sentence in the tool description is the lever. The current
description says: *"For stable concepts, definitions, or topics you
already know with confidence, answer directly without searching."*
That's the line to tighten if necessary.
