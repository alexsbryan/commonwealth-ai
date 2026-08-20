# Role cards — data, not code

One card per seat role. `scripts/co-role.py` is the only code that reads
them; a card change needs no code change, which is the point (ARCH §6 —
config as data).

**Why cards and not the skill.** `[models].context_size` was 16000 when
this was designed and the seat's monolithic standing prompt measures
~20.5k tokens (AGENTS.md 9.7k + SKILL.md 6.3k + charter 1.7k + contract
0.4k + boot block ~2k). It did not fit. Role decomposition is what makes
a local open-weight model viable here; it is not a style preference. R4
is the widest role at ~9.5k and fits on its own.

**Cards stay under ~800 tokens.** The failure being avoided is the cards
growing into a second constitution — which is the thing that already
does not fit. `co-role.py --lint` fails a card over the cap.

**Two measured constraints on card style**, both earned:

- Small local judges collapse into `could-not-judge` under hedge-heavy
  prompts (note `9aaac03b`: charter v5 taught a judge to read
  `[none provided]` as a CNJ cue and dropped dev 56.9% -> 38.4%). Write
  plainly. Do not stack qualifiers.
- This model emits an inline reasoning monologue before the JSON, so the
  harness takes the FIRST balanced JSON value and refuses a fragment.
  Cards do not need to ask for "no preamble"; the grammar handles shape
  and the parser handles the monologue.

## File format

YAML-ish frontmatter, then the card prose, then the schema as a fenced
`json` block. The prose is what the model sees; the schema is what
constrains it at decode time.

```
---
role: R5
name: integrate noise to backlog
gate: auto
engine: model
consumer: co-backlog.parse_item
schema: inline
---

<card prose>

```json
{ "type": "object", ... }
```
```

| key | meaning |
|---|---|
| `role` | `R1`..`R6` |
| `name` | the job, in the operator's words |
| `gate` | `draft` (operator approves) · `auto` (consumer-validated) · `charter` (R4's existing landing gate, unchanged) |
| `engine` | `model` (co-role.py calls the daemon) · `script` (co-role.py drives an existing script that owns the work) |
| `consumer` | the thing that accepts or rejects this role's output — every role has one, and the consumer IS the verifier |
| `schema` | `inline` (the fenced block) · `markers.verdict_schema` (R4, computed from the output contract) · `none` (script roles) |

## The design rule

**Every role's output already has a consumer in this repo**, and the
check is whether the consumer accepts it. No judge, no golden bank, no
kappa — the machinery is the verifier, and it either parses the output
or it does not. A role that cannot be checked mechanically says so and
returns could-not-judge. Nothing defaults to a pass.
