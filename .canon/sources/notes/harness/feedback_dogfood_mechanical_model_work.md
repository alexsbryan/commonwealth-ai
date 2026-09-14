# Mechanical model work (batch scoring, classification, summarization) routes to the Sovereign stack itself, not Claude tokens — dogfooding…

Operator directive (2026-08-09, after the backlog worker spent ~350k
Claude tokens, much of it on mechanically scoring 79 notes): "For
mechanical model work that's what our actual system is for — we
should solve problems with our own tools (dogfooding benefits) as
much as reasonably possible."

Why: the product IS a local inference system (daemon :9741, model
zoo, eval/enrich/recipe batch machinery). Routing mechanical passes
through it is cheaper, and every internal use is a product test that
surfaces real defects (dogfooding). Claude tokens are for judgment
the local stack cannot do — order design, verification, adjudication.

How to apply: at order intake, when a deliverable contains a
mechanical model pass (score/classify/summarize N items), the Engine
recommendation must name the sovereign-native path (daemon batch via
eval/enrich/recipe/chat) or cite why it cannot serve — same rule as
[[feedback-ground-in-reuse-before-building]] / core principle 11,
applied to inference instead of code. See the backlog item filed
2026-08-09 for the skill amendment.
