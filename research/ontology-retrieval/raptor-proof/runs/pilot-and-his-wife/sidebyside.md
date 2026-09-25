# EI7 stage-0 — three-way side-by-side

runs: `research/ontology-retrieval/raptor-proof/runs/pilot-and-his-wife`
arms: bare, ablation, full (never-ran: ablation)
fabricated members: never-ran — no `--vocabulary` was given
snippets: WITHHELD — mesh_sharing = False in recipe.toml

A column names the run it read: the lowest-numbered run of that arm that measured the question. `no walk` = the row carries no `atlas_walk`, which is the truth for bare; it is not `walk ran, reached nothing`.

## nqa-pilot-and-his-wife-01 — k4_whole_story

> What profession is the main character of this story?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 1.000 | never-ran | 1.000 / 1.000 |
| gold members | + sailor | never-ran | + sailor |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br>Ib Mathisen (person)<br> (untyped)<br> (untyped)<br>Salve (person)<br>Irishman (person)<br>the navy (institution)<br> (untyped)<br>Madam Gjers (person)<br> (untyped)<br> (untyped)<br>Salve (person) —involves→  (untyped)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —configures→  (untyped)<br>Ib Mathisen (person) —involves→  (untyped)<br> (untyped) —involves→ Federigo (person)<br> (untyped) —involves→ Captain Beck (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-02 — k4_whole_story

> What is the main character's name?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 1.000 | never-ran | 0.500 / 0.500 |
| gold members | + salve<br>+ kristiansen | never-ran | + salve<br>- kristiansen |
| found / missed | 2 / 0 | never-ran | 1 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran | Ib Mathisen (person)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br>Irishman (person)<br> (untyped)<br> (untyped)<br>Henrik (person)<br>Ib Mathisen (person) —involves→  (untyped)<br> (untyped) —involves→ Federigo (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→ Captain Beck (person)<br> (untyped) —involves→ Nils Buvaagen (person)<br>Irishman (person) —involves→  (untyped)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped) —involves→ Elizabeth (person)<br>Henrik (person) —involves→  (untyped)<br>Henrik (person) —involves→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-03 — k4_whole_story

> Who is Salve in love with?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.000 | never-ran | 1.000 / 0.000 |
| gold members | + elisabeth | never-ran | + elisabeth |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Lieutenant Beck (person)<br> (untyped) —involves→ Captain Beck (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Federigo (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-04 — k4_whole_story

> Why doesn't Elisabeth marry Salve early on in the story?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.333 / 0.333 | never-ran | 0.333 / 0.333 |
| gold members | - attracted<br>- young<br>+ officer | never-ran | - attracted<br>+ young<br>- officer |
| found / missed | 1 / 2 | never-ran | 1 / 2 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife<br>[26] pilot-and-his-wife<br>[27] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→ Garvloit (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-05 — k4_whole_story

> Whom does Elisabeth choose to love?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.000 / 0.000 |
| gold members | - chooses<br>- salve<br>- officer | never-ran | - chooses<br>- salve<br>- officer |
| found / missed | 0 / 3 | never-ran | 0 / 3 |
| fabricated | never-ran | never-ran | never-ran |
| citations | — | never-ran | — |
| evidence path | no walk | never-ran | no walk |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-06 — k4_whole_story

> Why can't Elisabeth marry Slave right away?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.000 | never-ran | 0.500 / 0.500 |
| gold members | + sailed<br>+ without | never-ran | - sailed<br>+ without |
| found / missed | 2 / 0 | never-ran | 1 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife<br>[26] pilot-and-his-wife<br>[27] pilot-and-his-wife<br>[28] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br>Gjert (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped) —involves→ Old Jacob (person)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br>Gjert (person) —involves→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-07 — k4_whole_story  · closed-book excluded

> What happens when Salve returns 10 years later?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.500 / 0.000 | never-ran | 0.000 / 0.000 |
| gold members | - marries<br>+ elisabeth | never-ran | - marries<br>- elisabeth |
| found / missed | 1 / 1 | never-ran | 0 / 2 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br>Salve (person)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-08 — k4_whole_story

> Why were Salve and Elisabeth miserable after they married?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.333 | never-ran | 0.333 / 0.333 |
| gold members | - forgive<br>- hesitating<br>- marry | never-ran | - forgive<br>- hesitating<br>+ marry |
| found / missed | 0 / 3 | never-ran | 1 / 2 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife<br>[26] pilot-and-his-wife<br>[27] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —involves→ Old Jacob (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-09 — k4_whole_story

> How long did it take Salve and Elisabeth to forma a happy life together?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 1.000 | never-ran | 1.000 / 0.000 |
| gold members | - years | never-ran | + years |
| found / missed | 0 / 1 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife |
| evidence path | no walk | never-ran | Elizabeth (person)<br> (untyped)<br>Salve (person)<br> (untyped)<br> (untyped)<br>Gjert (person)<br>Merdoe (place)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-10 — k4_whole_story

> What is the moral of this story?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.333 / 0.333 |
| gold members | - confidence<br>- trust<br>- needed | never-ran | + confidence<br>- trust<br>- needed |
| found / missed | 0 / 3 | never-ran | 1 / 2 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br>the Yankee boatswain (person)<br> (untyped) —involves→ the Becks (institution)<br> (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→ Salve (person)<br>the Yankee boatswain (person) —involves→  (untyped)<br>the Becks (institution) —involves→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-11 — k4_whole_story

> What is this story about?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.500 / 0.000 | never-ran | 1.000 / 0.500 |
| gold members | - married<br>+ life | never-ran | + married<br>+ life |
| found / missed | 1 / 1 | never-ran | 2 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br>the Yankee boatswain (person)<br> (untyped)<br> (untyped)<br>Stars and Stripes (place)<br> (untyped)<br> (untyped)<br>Ib Mathisen (person)<br> (untyped)<br> (untyped)<br>the Naiad (work)<br>the Yankee boatswain (person) —involves→  (untyped)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Gjert (person)<br>Stars and Stripes (place) —involves→  (untyped)<br> (untyped) —involves→ Federigo (person)<br> (untyped) —involves→ Nils Buvaagen (person)<br>Ib Mathisen (person) —involves→  (untyped)<br> (untyped) —involves→ crew (person)<br> (untyped) —involves→ Fru Beck (person)<br> (untyped) —involves→ harbour police (person) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-12 — k4_whole_story

> Who is this story about?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.500 / 1.000 |
| gold members | - pilot<br>- wife | never-ran | - pilot<br>+ wife |
| found / missed | 0 / 2 | never-ran | 1 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife |
| evidence path | no walk | never-ran | the Yankee boatswain (person)<br> (untyped)<br>Ib Mathisen (person)<br> (untyped)<br>Stars and Stripes (place)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br>the Naiad (work)<br>the Becks (institution)<br> (untyped)<br>the Yankee boatswain (person) —involves→  (untyped)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→ Salve (person)<br>Ib Mathisen (person) —involves→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Gjert (person)<br>Stars and Stripes (place) —involves→  (untyped)<br> (untyped) —involves→ Nils Buvaagen (person)<br> (untyped) —involves→ Fru Beck (person)<br> (untyped) —involves→ Widow Kirstine (person)<br>the Becks (institution) —involves→  (untyped)<br>the Becks (institution) —involves→  (untyped)<br> (untyped) —involves→ Federigo (person) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-13 — k4_whole_story

> What is the sailor describing in this story?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.333 | never-ran | 0.667 / 1.000 |
| gold members | + experiences<br>+ stormy<br>+ deep | never-ran | + experiences<br>+ stormy<br>- deep |
| found / missed | 3 / 0 | never-ran | 2 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife<br>[26] pilot-and-his-wife<br>[27] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br>the Yankee boatswain (person)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br>man-of-war cadet cruise (concept)<br> (untyped)<br>Juno (work)<br> (untyped)<br> (untyped)<br>Juno (work) —configures→  (untyped)<br>the Yankee boatswain (person) —involves→  (untyped)<br> (untyped) —involves→ Gjert (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —involves→ Nils Buvaagen (person)<br> (untyped) —involves→ Elizabeth (person)<br>Juno (work) —involves→  (untyped)<br>Juno (work) —involves→  (untyped)<br>Juno (work) —involves→  (untyped)<br>Juno (work) —involves→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-14 — k4_whole_story  · closed-book excluded

> Who is loved in return?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.000 | never-ran | 1.000 / 0.000 |
| gold members | + elisabeth | never-ran | + elisabeth |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife<br>[26] pilot-and-his-wife<br>[27] pilot-and-his-wife<br>[28] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Lieutenant Beck (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Captain Beck (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-15 — k4_whole_story  · closed-book excluded

> Why did Salve leave his land?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.500 / 0.500 | never-ran | 1.000 / 0.500 |
| gold members | + leaves<br>- desperation | never-ran | + leaves<br>+ desperation |
| found / missed | 1 / 1 | never-ran | 2 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Captain Beck (person)<br> (untyped) —involves→ Irishman (person)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→ Kristiansen (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-16 — k4_whole_story  · closed-book excluded

> Who does Elisabeth marry?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.500 | never-ran | 0.500 / 0.500 |
| gold members | + marries<br>+ salve | never-ran | - marries<br>+ salve |
| found / missed | 2 / 0 | never-ran | 1 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-17 — k4_whole_story  · closed-book excluded

> How many years elapse before an understanding is made?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 1.000 | never-ran | 1.000 / 1.000 |
| gold members | + years | never-ran | + years |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-18 — k4_whole_story  · closed-book excluded

> What makes Salve decide to finally marry Elisabeth?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.000 | never-ran | 1.000 / 0.000 |
| gold members | + true | never-ran | + true |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife<br>[26] pilot-and-his-wife<br>[27] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→ Captain Beck (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-19 — k4_whole_story

> What creates their happy life together?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.000 / 0.000 |
| gold members | - foundation | never-ran | - foundation |
| found / missed | 0 / 1 | never-ran | 0 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | — | never-ran | — |
| evidence path | no walk | never-ran | no walk |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-20 — k4_whole_story

> What is emphasized by this novel?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.000 / 0.000 |
| gold members | - confidence<br>- trust | never-ran | - confidence<br>- trust |
| found / missed | 0 / 2 | never-ran | 0 / 2 |
| fabricated | never-ran | never-ran | never-ran |
| citations | — | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped)<br>Exploits of Danish and Norwegian Naval Heroes (work)<br> (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-21 — k4_whole_story  · closed-book excluded

> What does a marriage need?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.333 | never-ran | 0.667 / 0.667 |
| gold members | - implicit<br>- confidence<br>- trust | never-ran | - implicit<br>+ confidence<br>+ trust |
| found / missed | 0 / 3 | never-ran | 2 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife<br>[21] pilot-and-his-wife<br>[22] pilot-and-his-wife<br>[23] pilot-and-his-wife<br>[24] pilot-and-his-wife<br>[25] pilot-and-his-wife<br>[26] pilot-and-his-wife<br>[27] pilot-and-his-wife<br>[28] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped) —involves→ Fru Beck (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Federigo (person)<br> (untyped) —involves→ tavern-keeper (person)<br> (untyped) —involves→ Madam Beck (person)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-22 — k4_whole_story

> Who does Salve Kristiansen love?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.000 | never-ran | 1.000 / 0.000 |
| gold members | + elisabeth | never-ran | + elisabeth |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br>Kristiansen (person)<br>Salve (person)<br> (untyped)<br> (untyped)<br>Kristiansen (person) —involves→  (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Anne Herluf Andersen (person)<br> (untyped)<br>Salve (person) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-23 — k4_whole_story  · closed-book excluded

> Who is Elisabeth initially attracted to?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.500 / 0.500 |
| gold members | - young<br>- officer | never-ran | + young<br>- officer |
| found / missed | 0 / 2 | never-ran | 1 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Nils Buvaagen (person)<br> (untyped) —involves→ Old Jacob (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-24 — k4_whole_story  · closed-book excluded

> How many years elapse before Salve and Elisabeth understand each other?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.000 / 0.000 |
| gold members | - ten | never-ran | - ten |
| found / missed | 0 / 1 | never-ran | 0 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br>Elizabeth (person)<br> (untyped)<br>Salve (person)<br>Gjert (person)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-25 — k4_whole_story

> Who was true to Salve?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.000 | never-ran | 1.000 / 0.000 |
| gold members | + elisabeth | never-ran | + elisabeth |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Captain Beck (person)<br> (untyped) —involves→ Mother Kirstine (person)<br> (untyped) —involves→ Lieutenant Beck (person)<br> (untyped) —involves→ Federigo (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Beck (person)<br> (untyped) —involves→ the Yankee boatswain (person)<br> (untyped) —involves→ harbour police (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-26 — k4_whole_story

> Who is living a happy life?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 0.500 | never-ran | 0.500 / 0.000 |
| gold members | + salve<br>+ elisabeth | never-ran | - salve<br>+ elisabeth |
| found / missed | 2 / 0 | never-ran | 1 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br>Old Jacob (person)<br>The Son (person)<br> (untyped)<br> (untyped)<br>the grandmother (person)<br>Merdoe (place)<br>Old Jacob (person) —involves→  (untyped)<br> (untyped)<br>Old Jacob (person) —involves→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Fru Beck (person)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br>Old Jacob (person) —involves→  (untyped)<br> (untyped) —involves→ Madam Beck (person)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Federigo (person)<br> (untyped) —involves→ Kristiansen (person) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-27 — k4_whole_story  · closed-book excluded

> How many people are in wedlock?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 1.000 | never-ran | 1.000 / 1.000 |
| gold members | + two | never-ran | + two |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] raptor-pilot-and-his-wife<br>[6] raptor-pilot-and-his-wife<br>[7] raptor-pilot-and-his-wife<br>[8] raptor-pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br>Widow Kirstine (person)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Madam Beck (person)<br> (untyped) —involves→ Garvloit (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped) —involves→ Fru Beck (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Federigo (person)<br> (untyped) —involves→ tavern-keeper (person)<br>Widow Kirstine (person) —involves→  (untyped)<br>Widow Kirstine (person) —involves→  (untyped)<br>Widow Kirstine (person) —involves→  (untyped)<br>Widow Kirstine (person) —involves→  (untyped)<br>Widow Kirstine (person) —involves→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-28 — k4_whole_story  · closed-book excluded

> What is The Pilot and His Wife?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 1.000 / 1.000 | never-ran | 1.000 / 1.000 |
| gold members | + story | never-ran | + story |
| found / missed | 1 / 0 | never-ran | 1 / 0 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife |
| evidence path | no walk | never-ran | the officer's wife (person)<br> (untyped)<br> (untyped)<br>Kristiansen (person)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped) —configures→  (untyped)<br> (untyped) —involves→ Madam Beck (person)<br> (untyped) —involves→ Garvloit (person)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br>Kristiansen (person) —involves→  (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→ Captain Beck (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Carl Beck (person)<br> (untyped) —involves→ Fru Beck (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped)<br> (untyped) —transition→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-29 — k4_whole_story

> Who is beautiful?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.000 / 0.000 |
| gold members | - elisabeth | never-ran | - elisabeth |
| found / missed | 0 / 1 | never-ran | 0 / 1 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife<br>[17] pilot-and-his-wife<br>[18] pilot-and-his-wife<br>[19] pilot-and-his-wife<br>[20] pilot-and-his-wife |
| evidence path | no walk | never-ran | Marie (person)<br>Mina (person)<br> (untyped)<br>Madam Beck (person)<br> (untyped)<br> (untyped)<br>Mina Beck (person)<br> (untyped)<br>Carl Beck (person)<br> (untyped)<br> (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Mina (person) —involves→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br>Madam Beck (person) —involves→  (untyped)<br>Madam Beck (person) —involves→  (untyped)<br>Madam Beck (person) —involves→  (untyped)<br>Madam Beck (person) —involves→  (untyped)<br> (untyped) —involves→ Salve (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→ Fru Beck (person)<br>Mina Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br>Carl Beck (person) —involves→  (untyped)<br> (untyped) —involves→ Nils Buvaagen (person)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped)<br> (untyped) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml

## nqa-pilot-and-his-wife-30 — k4_whole_story  · closed-book excluded

> Where does Salve sail to?

| | bare | ablation | full |
|---|---|---|---|
| run | run-1 | never-ran — no `ablation` arm under the runs dir | run-1 |
| judge / kw | 0.000 / 0.000 | never-ran | 0.000 / 0.000 |
| gold members | - distant<br>- shores | never-ran | - distant<br>- shores |
| found / missed | 0 / 2 | never-ran | 0 / 2 |
| fabricated | never-ran | never-ran | never-ran |
| citations | [1] pilot-and-his-wife<br>[2] pilot-and-his-wife<br>[3] pilot-and-his-wife<br>[4] pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife | never-ran | [1] raptor-pilot-and-his-wife<br>[2] raptor-pilot-and-his-wife<br>[3] raptor-pilot-and-his-wife<br>[4] raptor-pilot-and-his-wife<br>[5] pilot-and-his-wife<br>[6] pilot-and-his-wife<br>[7] pilot-and-his-wife<br>[8] pilot-and-his-wife<br>[9] pilot-and-his-wife<br>[10] pilot-and-his-wife<br>[11] pilot-and-his-wife<br>[12] pilot-and-his-wife<br>[13] pilot-and-his-wife<br>[14] pilot-and-his-wife<br>[15] pilot-and-his-wife<br>[16] pilot-and-his-wife |
| evidence path | no walk | never-ran |  (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br> (untyped)<br>Juno (work)<br> (untyped)<br> (untyped)<br> (untyped) —involves→ Salve (person)<br>Juno (work) —configures→  (untyped)<br> (untyped) —involves→ Elizabeth (person)<br> (untyped) —involves→ Stars and Stripes (place)<br> (untyped) —involves→ Kristiansen (person)<br> (untyped) —involves→ the Yankee boatswain (person)<br> (untyped) —involves→ harbour police (person)<br>Juno (work) —involves→  (untyped)<br>Juno (work) —involves→  (untyped)<br>Juno (work) —involves→  (untyped)<br>Juno (work) —involves→  (untyped)<br> (untyped) —involves→ Irishman (person)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped)<br>Salve (person) —involves→  (untyped) |

snippets withheld — mesh_sharing = False in recipe.toml
