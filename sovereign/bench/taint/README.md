# Bug-finder flow-graph — a generalized data-dependence analyzer for the codebase

**Status:** working prototype (Python, uncommitted). Proves the mechanics end-to-end
on `commonwealth-ai`; not yet ported to `corpus-engine`, not yet wired to the verifier.
Prototype-then-port, same path the fact-base arc took (`bench/spec_eval` → `facts.rs`).

## Thesis

Bug-finding shouldn't be a bespoke detector per bug class. Generate **one** artifact —
a typed data-dependence graph over the whole codebase (the "untangled wires") — and make
every question a reachability query `reach(source_pred, sink_pred)` over it. Panic-taint,
injection-taint, and program slicing are the *same* traversal with different endpoints.
This is the IFDS decomposition (Reps–Horwitz–Sagiv): build the exploded graph once, query
it many ways. It is the missing general artifact next to `facts.json` / `capability_map.json`
/ `atoms.lance` — indeed a `facts.rs` construction-field fact is already a *one-step* def-use
edge; this generalizes it.

---

## 1. Mechanics (the depth)

Four modules, each one stage of the pipeline. Everything is tree-sitter for syntax +
the SCIP graph (`~/.svrnmesh/indexes/<corpus>/scip_graph.db`) for interprocedural edges.

### 1a. Flow-graph generation — `taint.py` + `flowgraph.py`

The graph. Nodes are value occurrences, keyed by SCIP qualified name:

| node | meaning |
|---|---|
| `QUAL::@p:<name>` | a parameter (carries type + source classification) |
| `QUAL::<name>` | a local binding |
| `QUAL::@sink:<line>:<kind>` | an expression sink (carries op text + guard verdict) |
| `QUAL::@ret` | the return value |
| `QUAL::@srccall:<line>` | an external-deserialize source call |

Edges mean **"b derives from a"** (data dependence):
- **intraprocedural** (tree-sitter, exact function spans — sidesteps SCIP's garbage
  `line_end`): `let`/assignment def-use, sink operands, return operands. The precision
  crux is `data_idents()` in `taint.py` — it walks an expression yielding only identifiers
  in *value position* (skips method names, field names, free-fn names, type paths), so
  `x.method(a)` contributes `x` and `a`, not `method`.
- **interprocedural** (SCIP call edges + positional arg matching): caller `arg_i` → callee
  `@p:param_i`. Only first-party function-call edges (`% 0.1.20 %().` on both endpoints),
  which excludes the module/structural refs that made naive reachability over-connect.

Scale: **5,179 functions → ~32,800 nodes / ~60,900 edges in ~4 s.**

### 1b. Input layer — `sources.py`

A **structural** taxonomy of untrusted ingress. Precision principle: *a parameter is not a
source by default.* It's a source only when a structural signal says untrusted data enters,
and **framework-injected params are excluded even on a handler** (the server controls
`State<>`/`Extension<>`/`AppHandle`, the user does not).

| kind | detector | conf |
|---|---|---|
| `http_extractor` | param typed `Json<>`/`Query<>`/`Path<>`/`Form<>`/`Bytes`/`Multipart` | high |
| `tauri_command_arg` | non-framework param of a `#[tauri::command]` fn (frontend) | high |
| `peer_bytes` | `&[u8]`/`Bytes` param in the mesh transport/gossip layer | high |
| `deserialize_ext` | `from_slice`/`from_reader` over external bytes (gated, not `from_str` on consts) | med |
| `cli_arg` | `env::args()`/clap | low |

Result: **449 structural sources** (285 tauri · 120 http · 43 deserialize · 1 peer), down
from **3,972** with the earlier name heuristic. Trust-severity axis (peer > remote-content >
imported-content > local-frontend) is designed but not yet applied — see burn-down.

### 1c. Sink model + guard analysis — `guard.py`

A sink is a potential panic (`unwrap`/`expect`, `arr[i]`/`arr[a..b]`, `step_by`/`chunks`,
`/`,`%`) or an injection op (`Command`/`fs`/`Path`/http). The guard layer decides, with
**three verdicts and no soft-fail** (mirrors the fact-base Corroborated/Drift/Unverifiable):

- **SAFE** — every offset provably in-bounds / non-zero / char-boundary.
- **UNGUARDED** — an offset has an unguarded panic shape (underflow `-`, zero-div `/`).
- **UNCERTAIN** — can't decide structurally → **hand to the verifier** (never dropped, never
  claimed a bug).

Guards recognised:
- **data-flow** (on the offset's derivation, following def-use): `.find`/`.rfind`/`.position`
  ⇒ char-boundary + `<len`; `.min(len)` ⇒ upper-bounded; `n = .read(&mut buf)` for `buf[..n]`
  ⇒ buffer-bounded; `saturating_sub`/`checked_sub` ⇒ no underflow; outer `.max(K≥1)` ⇒ non-zero
  (the exact `(x/n).max(1)` vs `x.max(1)/n` discriminator that shipped the production bug).
- **control-flow** (dominating): a dominating `if/while/match` condition (or a guard-and-return
  sibling) that checks `<base>.len()`/`<base>.is_empty()`.

### 1d. Query engine — `flowgraph.py`

`reach(source_pred, sink_pred)` runs BFS over the data-dep edges; `backward_slice(node)` is
the reverse cone (program slicing). The three demo queries share one graph:

- **PANIC** — sources = structural untrusted, sinks = `panic:*`.
- **INJECTION** — same sources, sinks = `inject` (`Command`/`fs`/`Path`/http). *Only the sink
  predicate changed.*
- **SLICE** — backward dependency cone of any node.

A new vuln class = one new predicate, zero new graph work.

---

## 2. Why it's robust (mechanics evidence)

**Known-answer unit tests — 12/12** (`guard.py` discriminator):

| input | verdict | class |
|---|---|---|
| `step_by(len.max(1)/20)` (the shipped bug, pre-fix) | UNGUARDED | zerodiv |
| `step_by((len/20).max(1))` (the fix) | SAFE | outer-max |
| `step_by(2)` · `s[..n.saturating_sub(1)]` · `s[..n.min(s.len())]` | SAFE | literal/satsub/clamp |
| `s[..n-1]` (bare) | UNGUARDED | underflow |
| `if x.len()>MAX { x[x.len()-MAX..] }` | UNCERTAIN | dominating len-guard |
| `x>0 && arr[x-1]` · `if x>0 { arr[x-1] }` | UNCERTAIN | positivity `&&` |
| `x==0 \|\| arr[x-1]` | UNCERTAIN | positivity `\|\|` (zero-check) |
| `for j in 1..=n { arr[j-1] }` | UNCERTAIN | loop-bound |
| `x>5 \|\| arr[x-1]` (not a lower bound) | UNGUARDED | no over-clear |
| `tier_counts[(tier as usize)-1]` · `v[v.len()-1]` (no guard) | UNGUARDED | genuine |

**The precision funnel (panic vertical, whole tree):**

```
grep         9,548 unwrap (+1,700 expect)     undifferentiated
 └ reachability                               plateaus ~40% fns, mostly FP
    └ structural sources          449         (was 3,972 name-heuristic)
       └ interproc taint → untrusted→panic wires  11
          └ sink guard → 7 SAFE · 4 UNCERTAIN · 0 UNGUARDED · 0 false bugs
   guard-as-lint (taint-independent)  44 → 17 → 9 → 6
      44 raw → 17 (control-flow len-guard) → 9 (loop-bound) → 6 (positivity &&/||)
      all 6 survivors are genuine unguarded shapes (calibrated by hand-read)
```

**Convergent guard frontier (calibrated, not asserted):** every guard idiom modeled removes
an FP class, and each removal was validated by reading the residual. Data-flow guards proved
the 7 live panic wires SAFE (incl. the `head[..n]` buffer-bound and `.find()`-derived slices).
Then dominating len-guard (44→17), loop-bound (17→9), and index-positivity `&&`/`||`-zero
(9→6) each cleared exactly the FP class hand-reading predicted. The 6 remaining all survived
four guard layers and are genuine — no local guard exists.

**Honest soundness caveats** (this is an approximation, not a proof engine):
- SCIP first-party call edges are *sparse* (~30k over ~16k fns; misses trait/generic/macro
  dispatch) → taint **under**-approximates (false negatives).
- No field-sensitivity (`req.a.b`), no return-edge or closure/iterator flow yet.
- Guard analysis is intraprocedural; a guard in a caller is invisible.
- Verdicts are candidates, not proofs — UNCERTAIN is the honest residual for the verifier.

---

## 3. Prioritized burn-down

Ordered to **prove the system robust first** (depth on mechanics), with representative code
findings as evidence the depth surfaces real things. Not full breadth by design.

### A. System robustness — trust the verdicts

- **✅ DONE · index-positivity guard** (`x>0 && arr[x-1]`, `x==0 || arr[x-1]`, `if x>0 {…}`).
  Cleared the `title.rs`/`citation.rs`/`frontmatter.rs` FP classes. (Bug found + fixed en route:
  tree-sitter node identity needs `.id`, not Python `is` — `.named_children`/`.parent` return
  distinct wrappers.)
- **✅ DONE · loop-bound guard** (`for j in 1..=n { arr[j-1] }`). Cleared the Levenshtein
  `b_chars[j-1]` class. Combined effect: lint 44 → 6, all survivors genuine.
- **P1 · injection sink-guard** (path sanitation: `canonicalize` + base-dir containment,
  `starts_with`). Today all 18 injection wires are UNCERTAIN purely because this isn't built.
- **P1 · `unwrap`/`expect` origin analysis** (is the `None`/`Err` locally reachable — post
  `is_some`, literal `Some`, `?`-guarded). Today all → UNCERTAIN.
- **P2 · dominating-guard soundness**: current check is textual (`<base>.len()` mention). Make it
  compare the actual bound/relation; add guard-and-return dataflow, not just siblings.

### B. System coverage / soundness (raises recall)

- **P1 · field-sensitivity** (`req.a.b` as distinct tainted facts) — the biggest recall lever;
  most real wires in this app thread through struct fields, which positional taint drops.
- **P2 · return-value edges + closure/iterator flow.**
- **P2 · trust-severity ranking** on sources (peer > remote-content > imported-content >
  local-frontend) so adversarial wires surface first.

### C. The verifier (turns UNCERTAIN into a decision)

- **P1 · wire the UNCERTAIN residual to the 122B adversarial verifier** — re-fetch the sink's
  evidence, prompt to *refute* ("construct the triggering input or explain why it can't fire;
  default REFUTED"), confirm only with a concrete trigger. Same collect-cheap/judge-strong split
  as `inner_chaos` rejudge.

### D. Durability

- **P2 · port the generator to `corpus-engine`** as a first-class artifact + code-intel query
  (sibling of `facts.rs`); expose `reach(source, sink)` as a tool.
- **P2 · known-answer test bank**: the 6 unit cases + an end-to-end run against the parent of
  `6c9b4fd6` (does the whole pipeline flag the historical sampling-stride panic?).

### Representative code findings (proof of depth — not a breadth sweep)

| # | severity | conf | site | shape |
|---|---|---|---|---|
| 1 | med | high (shape); invariant unproven | `sovereign-tools/src/atlas_postinstall.rs:608,615,616` | `tier_counts[(tier as usize)-1]` into `[0usize;6]` → panics if `tier==0` (underflow) or `tier>6` (OOB); nothing locally validates `tier ∈ 1..=6` |
| 2 | low | high (shape); empty-reachability unproven | `sovereign-cli-llm/src/bench_cmd/atlas.rs:517,519` | `by_len[0]` / `by_len[len-1]` panic if `build_tasks` is called with empty `chapters` |
| 3 | — (verifier) | untrusted-reachable, UNCERTAIN | `sovereign-desktop/.../import_commands.rs:858` and `sovereign-meshapp/src/wrapped.rs:512,523,525` | imported-archive / tauri path → `fs::read_dir`/`fs::write`/`create_dir_all`; path-sanitation unproven (needs injection guard §A) |
| 4 | — (verifier) | UNCERTAIN | `commonwealth-api/src/frontdoor.rs:3449` | `stripped[start..end]` — `end` find-derived, `start` unknown |

Findings 1–2 are genuine unguarded shapes (latent panics on out-of-range/empty input);
3–4 are honest UNCERTAINs awaiting the injection guard / verifier — *not* claimed as bugs.

---

## 4. How to run

```bash
# requires: python3 + tree_sitter + tree_sitter_rust; a built SCIP graph for the corpus
cd sovereign/bench/taint
python3 flowgraph.py          # builds the graph, runs the 3 demo queries + guard-lint
python3 taint.py <file.rs>    # per-file source/sink classification
```

Paths to `REPO` and the SCIP DB are hardcoded at the top of `flowgraph.py`/`taint.py` —
parameterize before porting.
