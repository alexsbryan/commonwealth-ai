# Mechanism / Reasoning-Fidelity Harness — HANDOFF

**Last updated:** 2026-06-06 (evening) · **Status:** Phases 0–3 done + live-validated; Phase 4 has 1 of 3 classes (aggregation). The orchestrator is now fully class-generic, early-stopping is wired, and a third reasoning class (corpus-grounded attribution) + fidelity cards are live.

> **START HERE if you're picking this up:** read §1 (what this is), §3 (current state), then §6.4 (what's left). The single most useful artifact is the **scoring-join witness** in §5 — the negative control's `d_agent` must be **exactly 0.000**; re-run a small dev battery and confirm it before changing anything.

> **⚠️ DAEMON / MODEL CAVEAT (this session):** the daemon's `config.toml` now pins `primary`=`FINAL-Bench_Darwin-36B-Opus-Q6_K`, `fast`=`...APEX-I-Compact` (a qwen-MoE that hits `Decode -3` on some paths). The handoff's original `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`/`Qwopus3.5-4B` are gone (stale mesh-registry entries, not locally loadable). Worse, this daemon's BYOM OICP adapter routes forced-choice by *latency class*, not model name — so `--models primary,fast` collapse onto **one slot** (identical output). Run **single-model** here (`--models primary`) until the daemon is reconfigured with distinct, name-routed slots. The exact `−0.331/−0.014` anchor is therefore unreproducible; the model-independent witness (control `d_agent == 0.000`) is what to check instead.

---

## 1. What this is

A **metamorphic-testing harness** that decides, per *reasoning class*, whether a frozen LLM reasons from the **causal mechanism** or from **memorized association with the label** — and does so cheaply enough that a model can be characterized once (cached), read free per query.

The method (per class): generate synthetic/mined **cases**, **perturb** them, and check that the agent's decision is **invariant** to identity-preserving changes (INV) and **responds in the structurally-predicted direction** to mechanism-feature changes (DIR), measured against a transparent **structural prior** (oracle). A **feature-stripped negative control** (the agent blindfolded to the features) *must fail* the sensitivity tests — that's the instrument-validity guard. Verdicts are **consistency, not correctness** (only a future real-holdout package tests correspondence to reality).

Reference class: **relocation under a wealth tax**. The whole point of the recent work is generalizing this one class into a **registry of classes** (attribution, identity, comparative, aggregation) and making each cheap.

**Two plans drove this** (both in `~/.claude/plans/this-is-a-fascinating-recursive-cake.md` — the file was overwritten; the *current* content is the efficient-multi-class-extension plan):
1. The decisive slice (the wealth-tax instrument) — **shipped**.
2. The efficient multi-class extension (Phases 0–4) — **in progress** (see §3).

Also see the memory file `~/.claude/projects/.../memory/project_mechanism_fidelity_harness_2026_06_05.md`.

---

## 2. Where the code lives

**Pure logic** (`sovereign/crates/sovereign-eval/src/mechanism_fidelity/`) — rebuilds/tests in seconds, no daemon:
- `case.rs` — `Case` (wealth-tax features + identity) + seeded `generate_cases` + `swap_identity`. **Invariant:** `narrative` must stay mechanism-free or the control leaks (guarded by `narrative_is_mechanism_free` test).
- `structural.rs` — `structural_p_relocate` (logistic prior); pins the 4 doc oracles (B≈0.954, P1≈0.008, P2/I1 flat).
- `perturb.rs` — `Variant{Base,DirP1,DirP2,InvI1}`, `PerturbKind`, `RenderMode{Full,Stripped}`, `render_prompt`.
- `score.rs` — `Bands`, `Scores`, `score(kind,d_agent,d_struct,bands)`, `ResultRow` (the Rust→Python contract). **Class-agnostic; do not special-case here.**
- `stopping.rs` — **(Phase 1)** empirical-Bernstein confidence interval at a pre-registered checkpoint schedule (Bonferroni α). `BoundedMean`, `StoppingConfig`, `decide() -> Verdict{Pass,Fail,Inconclusive,Continue}`, `Side{AtLeast,AtMost}`.
- `class.rs` — the `ReasoningClass` trait `{id, system_prompt, candidates, target_prob, build_probes}` + `RenderedProbe` (finished prompt + scoring metadata). `system_prompt` is per-class so the elicitation request is fully class-determined (and the wealth-tax wording stays byte-stable).
- `classes/wealth_tax.rs` — wealth-tax as the first registered class; reuses case/perturb/structural; anchors the A/B/C letter legend into each prompt.
- `classes/attribution.rs` — **(Phase 3)** corpus-grounded attribution-support: mines `Claim` atoms + evidence from `<corpus>/atlas/atoms.json`; exact 0/1 oracle; negate/reframe/distractor perturbations + blindfold control; `cheatable` guard + `control_cannot_cheat` test. **No lance dep** (uses the excerpt text in atoms.json).
- `classes/aggregation.rs` — **(Phase 4)** synthetic counting-under-a-threshold; exact 0/1 oracle; remove-below / add-above / rename perturbations + roster-withheld blindfold control.
- `registry.rs` — `registry()` / `by_id()` / `class_ids()` — three classes registered.
- `card.rs` — **(Phase 3)** `FidelityCard` + `grade_class` (mirrors `verdict.py` tiering) + per-model JSON I/O at `~/.sovereign/model-fidelity-cards/`.
- `mod.rs` — `Pool{Train,Dev,Test}` + re-exports. **Module is still named `mechanism_fidelity`** (the plan's rename to `reasoning_fidelity` is cosmetic and was deferred to avoid import churn).

**Inference-coupled orchestrator** (`sovereign/crates/sovereign-cli-llm/src/bench_cmd/mechanism_fidelity.rs`):
- `cmd_mechanism_fidelity` / `run` — CLI `sovereign bench mechanism-fidelity run`.
- `elicit` (K-sampling, legacy), `elicit_logprob` (**Phase 0**, the K-killer), `preflight`, `build_rows`, `print_glassbox_summary`, checkpoint/resume (`CheckpointAgg`, `load_checkpoint`, `open_checkpoint`, `append_checkpoint`).
- Registered in `bench_cmd/mod.rs` (`"mechanism-fidelity" => …`).

**Embedded forced-choice path** (`sovereign/crates/sovereign-inference/src/embedded/model_slot.rs`):
- `forced_choice_candidates(&CompletionRequest) -> Option<Vec<String>>` — detects the sentinel.
- `forced_choice_probs(model, ctx, candidates) -> Vec<(String,f32)>` — reads masked next-token logits over the candidate leading tokens (bare + space-prefixed), softmax.
- The branch in `generate_sync` right after the prefill (`*cached_tokens = tokens.clone();` … before `build_sampler`) + the MTP gate (`&& forced_choice_candidates(request).is_none()`).

**Config / verdict** (`sovereign/bench/mechanism_fidelity/`):
- `manifest.toml` — pre-registration: `[bands]`, `[stopping]`, `[negative_control]`, `[acceptance.*]`.
- `verdict.py` — stdlib-only tiered, power-annotated read of `ResultRow` JSONL.
- `results/` — run outputs. `baselines/mechanism_fidelity/peek_budget.json` — sacred-test ledger.
- `README.md`, this `HANDOFF.md`.

---

## 3. Phase status

| Phase | Status | Notes |
|---|---|---|
| **0 — forced-choice logprob** | ✅ **done + live-validated** | ~16× faster, deterministic, real finding, routing bug fixed (§7). Now **sequential** (determinism gate, §7). |
| **1 — early-stopping** | ✅ **done + wired + live-validated** | `stopping.rs` + `decide_at` + `[stopping]` manifest. Wired case-grouped per model; stops on **verdict determination** (any required band Fail → NO-GO; all Pass → GO), not "all four means terminal" (that never fired — flat/inv AtLeast-0.90 bands straddle). **Verified:** an n=200 dev run stopped at **64/200** cases (NO-GO, μ_mag Fail). |
| **2 — class registry** | ✅ **done + live-validated** | Orchestrator fully class-generic: `--class`/`--corpus`, class-driven `build_probes`/`candidates`/`target_prob`/`system_prompt`, string-keyed aggs+checkpoint, base-join by `(model,case,render,paraphrase)`. K-sampling dropped (`--no-logprob` errors). Control `d_agent == 0.000` reproduced → join intact. |
| **3 — attribution + cards** | ✅ **done + live-validated** | `classes/attribution.rs` (corpus-grounded: mines `Claim` atoms + evidence from `atlas/atoms.json`; exact 0/1 oracle; blindfold control + `control_cannot_cheat` test). `card.rs` → `FidelityCard` per `(model,class)` at `~/.sovereign/model-fidelity-cards/<model>.json`, manifest-fingerprinted. Live: attribution P1 Δ̄=−0.218, control 0.000; card accumulates all classes. |
| **4 — more classes** | ◑ **1 of 3 (aggregation done)** | `classes/aggregation.rs` (synthetic counting-under-threshold; exact 0/1 oracle; live P1 Δ̄=−0.868 on Darwin — strong count-tracking). **Remaining: comparative + identity** (§6.4) — same trait shape; see notes. |

Tests green right now: **41** in `sovereign-eval` (`cargo test -p sovereign-eval mechanism_fidelity`) + **3** in cli-llm. Full-workspace lint clean (0 errors). The harness now characterizes a model across a *spectrum* of reasoning classes — Darwin-36B (this daemon's `primary`) graded **counting** strong (−0.868), **attribution** moderate (−0.218), **wealth-tax** none (−0.006, NO-GO/Unfaithful).

---

## 4. The big idea on efficiency (why Phase 0 matters)

Naive cost = classes × cases × variants × **K draws** × (1/throughput). The plan kills each multiplier:
- **K draws → 1** via forced-choice **logprob** (no logprobs existed; we read the masked first-token distribution in one pass). ✅ done — the ~16× win.
- **fixed n → early-stopping** (empirical-Bernstein CS at pre-registered checkpoints). ⚠️ core done, not wired.
- **hand-authored → corpus-mined** cases (Phase 3+).
- **per-query → one-time cached card** (Phase 3 `FidelityCard`).
- **single-box → mesh fan-out** (later).

The contained trick that made Phase 0 cheap to ship: the forced-choice request rides inside `structured_output` as a sentinel `{"type":"string","enum":[...],"x_forced_choice":true}` — a valid schema that is **only ever compiled in `build_sampler`**, which the branch bypasses. So it traverses the existing HTTP plumbing untouched → **zero** changes to `completion.rs`, `remote.rs`, the daemon adapter, or the 35 `CompletionResponse` construction sites. The probs come back as JSON in `text`.

---

## 5. THE WITNESS — run this first

> The original two-model anchor (`primary −0.331 / fast −0.014`) is **unreproducible** on this daemon — those models are gone and `primary`/`fast` now collapse to one slot (see the top caveat + §7). The durable, **model-independent** check is the scoring-join witness: **the negative control's `d_agent` must be exactly `+0.000`** (byte-identical blindfold prompts + deterministic sequential elicitation). If it isn't, the base-join or determinism broke.

```bash
# (daemon must be up + running the forced-choice code — see §8)
./target/debug/sovereign-cli-llm bench mechanism-fidelity run \
  --models primary --pool dev --n-cases 30 --seed 0 \
  --manifest sovereign/bench/mechanism_fidelity/manifest.toml \
  --out sovereign/bench/mechanism_fidelity/results/oracle.jsonl
python3 sovereign/bench/mechanism_fidelity/verdict.py \
  sovereign/bench/mechanism_fidelity/results/oracle.jsonl \
  --manifest sovereign/bench/mechanism_fidelity/manifest.toml
```

**Expected:** `control P1 Δ̄ = +0.000` (exactly), `ctrlDir 0%`, `0 failures`. The headline P1 Δ̄ is now model-dependent — on this daemon's `primary` (Darwin-36B) wealth-tax shows **no** collapse (P1 ≈ −0.006 → NO-GO/Unfaithful), which is a genuine finding. To see the harness's *range*, run the other two classes (both stop fast at small n):

```bash
# corpus-grounded attribution (any indexed corpus with Claim atoms)
... run --class attribution_support --corpus ~/.sovereign/indexes/commonwealth-ai-system-overview --models primary --n-cases 16 ...
# synthetic aggregation (counting)
... run --class aggregation_threshold --models primary --n-cases 16 ...
```
On Darwin-36B these gave P1 Δ̄ ≈ −0.218 (attribution) and ≈ −0.868 (aggregation) — a clean fidelity *spectrum*, all with control 0.000. Cards land in `~/.sovereign/model-fidelity-cards/primary.json`.

If you change the orchestrator and **control Δ̄ moves off 0.000**, you broke the scoring join — debug before proceeding.

---

## 6. WHAT'S DONE (this session) + WHAT'S LEFT

§6.1–§6.3 below are **done + live-validated**; §6.4 has aggregation done, comparative + identity remaining.

### 6.1 Orchestrator generalization — ✅ DONE
The orchestrator (`bench_cmd/mechanism_fidelity.rs`) is fully class-generic: `--class`/`--corpus` resolve a `ReasoningClass`; probes come from `class.build_probes(n, seed, corpus)`; `elicit_logprob` builds the sentinel from `class.candidates()`, parses `parse_forced_choice_dist`, and maps via `class.target_prob`; the system prompt comes from `class.system_prompt()` and the prompt is already letter-anchored (no legend appended). Aggregates + checkpoint are string-keyed `model|case|render|paraphrase|variant`; `build_rows` joins each probe to its base by `(model,case,render,paraphrase)` and scores `rp.kind`. `class` + `n_drawn`/`stopped_early`/`cs_lower`/`cs_upper` added to `ResultRow`. K-sampling removed (`elicit`/`parse_decision`/`decision_schema` gone; `--no-logprob` errors; `--logprob` is a no-op default). **Elicitation is SEQUENTIAL** (see §7 — the control-determinism invariant). Verified: control `d_agent == 0.000` reproduced on every class.

### 6.2 Early-stopping — ✅ DONE + WIRED
`StoppingConfig` loaded from `[stopping]`; elicit loop is case-grouped per model; after each case the four `BoundedMean`s are updated (`μ_mag` DIR-P1 magnitude_ok among large-Δ, `μ_flat_p2`, `μ_inv`, `μ_ctrl`) and evaluated with **`decide_at`** (checkpoint gated on the **case counter**, because `μ_mag` only accrues on large-Δ cases so its own `n` lags and would skip the checkpoint values). **Key design change vs the original spec:** stop on **verdict determination** — any required band `Fail` → NO-GO, all `Pass` → GO — *not* "all four means terminal" (the flat/inv AtLeast-0.90 bands straddle indefinitely on a good-but-imperfect model, so that condition never fired; an n=200 run ground to 173 cases without stopping). Train/Dev only; Test fixed-n. **Verified:** an n=200 dev run on `primary` stopped at **64/200** (NO-GO, μ_mag Fail, `stopped_early=true`).

### 6.3 Attribution class + fidelity cards — ✅ DONE
`classes/attribution.rs` impl `ReasoningClass` (`["A","B"]`, `target_prob=P(A)`): `build_probes` loads `<corpus>/atlas/atoms.json`, mines `Claim` atoms with non-empty `evidence` + a substantive `quotable_excerpt`, and excludes `cheatable` claims (excerpt ⊆ claim) so the blindfold control can't self-verify. Each claim → base(supported, oracle 1.0) + dir_p1(evidence negated → 0.0, sign −1) + dir_p2(distractor appended → 1.0, flat) + inv_i1(passage reframed → 1.0) + a stripped **blindfold control** (passage withheld → base/dir_p1/dir_p2 prompts byte-identical). **No lance dependency** — the excerpt in atoms.json is the evidence text. `card.rs`: `FidelityCard` (`Grade{Faithful,Unfaithful,ControlLeak,Inconclusive}` + metrics, grading mirrors `verdict.py`) written per `(model,class)` to `~/.sovereign/model-fidelity-cards/<model>.json`, stamped with a manifest fingerprint (stale bands invalidate). The orchestrator writes/merges a card per model after each run. Live: attribution P1 Δ̄=−0.218, control 0.000, card written. **NOTE:** the real `Claim` schema is `{content, evidence:[{chunk_id,passage_preview}], quotable_excerpt, …}` (atoms.json `{atoms:[{atom_type,data}]}`), NOT the `corpus-engine/.../atoms.rs` path the old §6.3 cited.

### 6.4 — REMAINING: comparative + identity classes
Aggregation is **done** (`classes/aggregation.rs`, synthetic counting-under-threshold, exact 0/1 oracle, blindfold control — live P1 Δ̄=−0.868). Each remaining class is one `ReasoningClass` impl emitting the same `base/dir_p1/dir_p2/inv_i1` × `full/stripped` shape (so scorer/verdict/cards/early-stopping all work unchanged):
- **Comparative** — corpus-grounded on `EdgeType::Tension` + `Position.salience`: base = "does A oppose B?"; dir = strengthen/weaken the tension; **INV = order-invariance** (swap A/B → same answer); blindfold control hides the positions. (Atom structs live in `corpus-engine` atlas; confirm field names against a real `atoms.json` first, as the attribution path showed the docs drift.)
- **Identity** — `entity_resolution_bench::GroundTruthEntity` + a deterministic name-paraphrase engine (~200 LOC): base = "are these two mentions the same entity?"; dir = swap a discriminating attribute (→ different); INV = paraphrase the name (→ same); blindfold control hides the attributes.
- Temporal + argument-structure remain stretch (temporal needs date extraction).
Each must add the analogous **"control can't cheat" test** (the §7 invariant) and a deterministic-in-seed test.

---

## 7. Gotchas / invariants (READ before editing)

- **`request.model_id` MUST be set per call.** `RemoteApiProvider::build_request` (`remote.rs` ~line 117) routes on `request.model_id`, NOT the provider's own id — `None` ⇒ empty model field ⇒ daemon's *default* slot. Symptom: multi-model runs silently serve every model from one slot; deterministic logprob exposes it as bit-identical results, sampling noise hides it. This contaminated the original K-sampling pilot. `elicit_logprob` sets it.
- **Elicitation MUST be sequential (determinism = control validity).** The negative control's "provably blind" guarantee rests on `stripped(base)` and `stripped(perturbed)` being byte-identical → identical logprobs → `d_agent == 0`. Concurrent same-slot requests get batched, and the daemon's batched matmul reductions are **not bit-invariant to batch composition** — so two byte-identical prompts in different batches return slightly different logits, and control `d_agent` drifts off 0 (observed `−0.024`/`+0.012` under `--concurrency 8`). It also breaks the deterministic-peeking premise of early-stopping. The orchestrator now elicits one probe at a time; `--concurrency >1` is accepted but ignored (with a note). **The model-independent regression witness is `control d_agent == 0.000`** — check that, not absolute numbers.
- **This daemon collapses models by latency class.** With the current BYOM `config.toml` (`primary`=Darwin-36B, `fast`=APEX-Compact), the daemon's OICP slot picker routes forced-choice on *latency class*, not model name, so `--models primary,fast` return identical distributions (one slot). Multi-model comparison needs a daemon configured with distinct, name-routed slots; until then run single-model. (`/v1/models` may also list stale **mesh-registry** models that aren't locally loadable — "no node advertises" — don't target those.)
- **Daemon restart applies `config.toml`, not the running set.** A long-lived daemon can be serving models that no longer match `~/.sovereign/config.toml`; `sovereign daemon restart` loads the on-disk config (and the 35B needs ~33–60s). Don't restart to "clear a degraded slot" expecting the same models — check `config.toml` first. (A degraded slot is usually *self-inflicted concurrency overload*; sequential elicitation avoids it. `elicit_logprob` retries 4× with backoff for genuine transient MTP-503s.)
- **Negative-control validity rests on `narrative` being mechanism-free** + the stripped render hiding features, so stripped(base) and stripped(perturbed) prompts are byte-identical → the control is provably blind. Any new class must preserve this (add the analogous "control can't cheat" test).
- **The forced-choice sentinel must stay a valid JSON schema** (so nothing chokes if something does compile it) and the branch must sit **before `build_sampler`**.
- **n=16 is a warmup** in early-stopping (empirical-Bernstein additive term ~0.82 there); first decisive checkpoint ≈ 32.
- **`verdict.py` power line is wrong for logprob.** It prints binomial SE (`0.707/√K`) which assumes sampling; logprob is deterministic (no per-probe variance). The `±sem` column (across-case) is the correct uncertainty. Small fix: detect logprob mode (eff_k==1, deterministic) and report only the across-case SEM. Low priority.
- **`finding`:** the verdicts so far are NO-GO because these *local* models are weakly/not mechanism-faithful on the synthetic wealth-tax task (35B −0.331, 4B −0.014) AND the base relocate-rate is low (~0.32), leaving little room to collapse — this is a real result, not a bug (control is perfectly valid). A frontier model (deferred, needs an API key) is both the strong-model witness and the throughput path.

---

## 8. Build / run / daemon mechanics

- **Watcher is down** this session → running `cargo` directly is fine (no lock contention). Per-crate during iteration; full workspace at a phase boundary.
- **Debug builds** (per `feedback_use_debug_builds`): `cargo build -p <crate> --bin <bin>` → `target/debug/<bin>`.
- **Editing the embedded path** (`model_slot.rs`/`sampler.rs`) requires rebuilding **and restarting the daemon**: the running daemon is `target/debug/sovereign-cli-daemon daemon run` (serves `:9741`, links `sovereign-inference`).
  ```bash
  cargo build -p sovereign-cli-daemon --bin sovereign-cli-daemon
  sovereign daemon restart        # then poll /v1/models — ready in ~33s
  ```
  Poll loop (foreground `sleep` is blocked by the harness — use `run_in_background`):
  ```bash
  for i in $(seq 1 50); do curl -s --max-time 3 localhost:9741/v1/models >/dev/null 2>&1 && { echo up; break; }; sleep 3; done
  ```
- **Quick forced-choice sanity via curl** (proves the daemon routes per-model):
  ```bash
  curl -s localhost:9741/v1/chat/completions -H 'content-type: application/json' -d '{"model":"primary","messages":[{"role":"user","content":"... Answer with one letter — A=relocate, B=stay, C=indifferent."}],"max_tokens":1,"response_format":{"type":"json_schema","json_schema":{"name":"s","schema":{"type":"string","enum":["A","B","C"],"x_forced_choice":true}}}}'
  # → message.content == {"A":0.87,"B":0.08,"C":0.04}
  ```
- **Definition of done** (per CLAUDE.md): `lint_status` + `test_status` both `fresh_passing`, or `scripts/sovereign-{lint,test}.sh --human` if watcher down. NOTE: 3 pre-existing workspace test failures live in untouched crates (`sovereign-server` ws_streams, `corpus-engine` snapshot + uap e2e) — not ours.

---

## 9. Quick task list for the next session
1. `cargo test -p sovereign-eval mechanism_fidelity` (**41** green) + the cli-llm test (3) → confirm.
2. Run the §5 **witness** (single-model) → confirm `control Δ̄ == +0.000` and 0 failures. (Don't expect the old `−0.331/−0.014` — see the top caveat.)
3. **§6.4 remaining classes** — comparative + identity (each one `ReasoningClass` impl in the `base/dir_p1/dir_p2/inv_i1 × full/stripped` shape; add the "control can't cheat" + deterministic-in-seed tests). Confirm atom field names against a real `atoms.json` before coding (the docs drift — attribution proved it).
4. **Daemon hygiene (blocks multi-model science):** reconfigure the daemon with ≥2 distinct, name-routed forced-choice slots so `--models a,b` don't collapse to one slot, then re-run a 2-model battery. The frontier-model witness (needs an API key via `--base-url`/`--api-key-env`) is the strong-faithful comparison point.
5. **Read-side card integration** (deferred package): a router/chip that loads `~/.sovereign/model-fidelity-cards/<model>.json`, checks `manifest_fingerprint`, and gates trust per class.
6. Optional: rename module `mechanism_fidelity` → `reasoning_fidelity` (cosmetic; import churn).
