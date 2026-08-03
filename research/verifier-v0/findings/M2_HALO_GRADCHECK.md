# M2 — the Halo lane trains. Gate passed 2026-08-02.

Answers `HALO_HANDOFF_2026-08-02.md` §4 ("prove the lane actually updates
weights") and closes §8's first open item.

---

## Bottom line

**Vanilla TRL on gfx1151 trains all 24 layers of Qwen3.5-0.8B, gated deltanet
included.** 186/186 LoRA B matrices left zero after five optimizer steps; the
Mac's MLX lane could reach exactly one layer. **Unsloth is therefore an
optimisation, not a prerequisite** — the handoff's §3 recommendation can be
descheduled from the critical path and run later as a throughput experiment.

Two corrections to the handoff fall out of this, one of them blocking:

1. **`train_orpo_trl.py` never saved weights** (`save_strategy="no"`, no save
   after `trainer.train()`). §4's gate could not have passed as written — it
   would have reported "no .safetensors found". Fixed; every run now writes an
   adapter and states its own verdict.
2. **The long-run blocker is memory, not correctness.** The gate says nothing
   about the GTT ratchet that killed the M0 run at step 63, and that ratchet
   still bounds the mix study. See "What is still blocked".

---

## The measurement

`runs/halo-gradcheck/`, 5 steps, micro-batch 1, grad-accum 1, seq 1024,
lr 1e-4, r=32/α=64, β=0.1, seed 17, `data/orpo-probe`. Wall clock 16 s.

| | |
|---|---|
| backend | `trl-vanilla` (trl 1.9.2, peft 0.20.0, transformers 5.14.1) |
| container | **`sovereign-vulkan`** — not `sovereign-rocm-7.2.4` (see below) |
| torch | 2.10.0+rocm7.0, hip 7.0.51831, gfx1151, 124 GB unified |
| deltanet path | `sequential` (fla present, causal-conv1d absent — correct) |
| adapted modules | 186, matching the Mac probe's 186 exactly |
| loss | 1.630 → 0.806 over 5 steps |

### The gate

```
lora_A tensors  186   max|A| 3.141624e-02
lora_B tensors  186   max|B| 3.007761e-04   nonzero 186/186
TRAINED -- the adapter changes the model
```

Run twice, independently: once inline by the trainer, once by
`scripts/check_adapter_trained.py` against the saved directory, so a bug in the
trainer's own gate cannot vouch for the trainer.

### B by layer type — the part that matters

MLX's `Primitive::vjp Not implemented for CustomKernel` made 18 of 24 layers
undifferentiable, leaving one reachable layer. Here every projection moved:

| leaf | n | nonzero | max\|B\| | kind |
|---|---|---|---|---|
| `gate_proj` / `up_proj` / `down_proj` | 24 ea | 24 | 3.008e-04 | mlp |
| `in_proj_qkv` / `in_proj_z` / `in_proj_b` / `in_proj_a` / `out_proj` | 18 ea | 18 | 2.999–3.007e-04 | **gated deltanet** |
| `q_proj` / `k_proj` / `v_proj` / `o_proj` | 6 ea | 6 | 3.005–3.007e-04 | self_attn |

**Layers with nonzero B: 24/24. Layers still at exactly zero: none.**

The uniform ~3.0e-4 magnitude is expected, not suspicious: AdamW normalises the
update to ~unit scale, so after N steps from B=0 the displacement is ≈ lr × N
modulated by warmup — 1e-4 × 5 ≈ 3e-4. It is reachable *only* with a nonzero
gradient; a zero gradient yields exactly 0, which is the whole basis of the gate.

---

## The Halo did not need its own container

The training stack runs **inside `sovereign-vulkan`**, which has no `/opt/rocm`
at all. Verified 2026-08-02: bf16 matmul, autograd backward, and a hand-written
Triton kernel compiled and executed on gfx1151.

The requirement is not a system ROCm install. It is (a) the kernel driver —
`/dev/kfd`, host-level — and (b) *some* HSA runtime preloaded, because torch's
bundled ROCm 7.0 runtime SIGSEGVs against this driver and torch's RPATH beats
`LD_LIBRARY_PATH` (note `b18dacf9`). Two work identically:

- `/opt/rocm/lib/libhsa-runtime64.so.1` — `sovereign-rocm-7.2.4` (1.18.70204)
- `/run/host/usr/lib64/libhsa-runtime64.so.1` — the Fedora host's, visible from
  any toolbox (1.18.0)

`launch_gradcheck.sh` detects rather than hardcodes, so it runs in either
container. This also removes the operator dependency: an agent session started
in the vulkan toolbox can drive training without a second terminal, and nested
`toolbox run` is impossible from inside a toolbox (`flatpak-spawn(1) not found`).

**Corollary for §3:** Triton ships its own `libhsa-runtime64.so` and AMDGPU
LLVM backend in the wheel, so Unsloth's "pip-packaged ROCm nightlies, no
system ROCm" claim is consistent with what we measured. It was never the
blocker.

---

## What this settles in the handoff

- **§4** — passed. Both defects that produced it were Mac/MLX-specific and
  neither exists on this lane.
- **§8, "Whether ORPO + LoRA + gated deltanet trains on gfx1151"** — yes, under
  vanilla TRL. Unsloth remains unverified and is now unnecessary to verify first.
- **§2's reframe is confirmed from the other direction.** The M0 run's 176.71
  s/it was real work: `grad_norm` ran 2.431 → 0.30 across all 63 steps, never
  zero. The Mac's ~53 s/it was a forward pass with no backward. The two numbers
  were never comparable.
- **The 54.2 / 55.03 base-model reference stands** and is still the control the
  mix study needs.

---

## The gate itself was broken, and the 25-step run is what caught it

A 25-step follow-up at identical settings **failed** the gate with
`NOT TRAINED -- B is exactly zero`. It was not zero. **All 372 tensors were
NaN**, and the gate could not tell the difference:

```
max(0.0, float('nan')) == 0.0      # NaN loses every comparison,
float('nan') > 0.0     == False    # so it can never become the running max
```

So a numerically unstable run — a *real* trainer doing *real* work at too high
an effective learning rate — was reported with the exact words reserved for a
structurally dead one. That is the worst possible confusion for this project:
**NOT TRAINED means fix your framework; DIVERGED means fix your LR.** Acting on
the wrong one sends the next session back down the MLX rabbit hole to debug a
gradient path that was never broken.

`check_adapter_trained.py` now checks finiteness *before* any `max()`, and has
three verdicts with distinct exit codes — 0 TRAINED, 1 NOT TRAINED, 3 DIVERGED,
2 unusable. Verified in all three directions: the gradcheck adapter (0), the
NaN adapter (3), and a synthetic random-A/zero-B adapter reproducing the MLX
fingerprint (1). `train_orpo_trl.py` now imports `scan()` from it rather than
carrying a second copy of the rule.

## The divergence — root-caused: §4's own command trips §7's trap

**`--seq-len 1024` truncates 92.5% of the training prompts, and that is what
diverges the run.** TRL sets `max_prompt_length = seq_len // 2`, so seq 1024
caps prompts at **512 tokens** against a `data/orpo-probe` distribution of
p50 819 / p90 1251 / max 1758 (n=400, measured with the model's own tokenizer):

| prompt cap | seq_len | prompts truncated |
|---|---|---|
| 512 | 1024 | **370/400 = 92.5%** |
| 2048 | 4096 | **0/400 = 0.0%** |

Three runs, one variable:

| config | outcome | gate |
|---|---|---|
| seq 1024, accum 1, 5 steps | healthy, loss 1.63 → 0.81 | **PASS** |
| seq 1024, accum 1, 25 steps | **NaN at step 11** | DIVERGED |
| seq 1024, accum 8, 15 steps | **NaN at step 2** | DIVERGED |
| **seq 4096, accum 1, 15 steps** | clean, `grad_norm` 1.9–5.8, never NaN | **PASS**, max\|B\| 8.005e-04 |

I first assumed effective batch 1 was the culprit — ORPO's log-odds term at its
noisiest. **That was wrong, and raising the batch refuted it**: accum 8 diverged
*sooner* (step 2, not step 11), because each optimizer step then averages eight
truncated examples instead of one. Seq length was the variable that mattered.

This makes the handoff self-contradictory, and **§7 is the side that is right**:
§4 prescribes `--seq-len 1024` while §7 warns that `max_prompt_length` is live
for TRL and truncates the evidence the verifier is meant to check. A gate must
not run a config the real training never uses. `launch_gradcheck.sh` now
defaults to **seq 4096, 15 iters, gradient checkpointing on** (checkpointing was
measured free on this box, and holds peak to 20.3 GB at 7.2 s/it).

**Five steps is a liveness check, not a stability test.** The 5-step run passed
one step before the same config went NaN. Both runs are honest; they answer
different questions. The default is now 15.

**M0 is unaffected** — it ran at seq 4096 and never truncated, which is why it
went 63 steps with `grad_norm` steady at ~0.31. The mix study is likewise
unaffected: `findings/truncation_report.json` already establishes 4096 as the
safe cap (Stream A 0.017%, Stream B 0.000%).

---

## The GTT question: the instrument was wrong, and the named mitigation is dead

`M0_PROBE_HALO.md:60-83` had already established the curve, the onset, the
hand-done attribution, the fragmentation reading, the `expandable_segments`
gap, and a named-but-untested mitigation. It deferred the test for a stated
reason — *"costs a ~5 h run… would not change the role assignment — at 3.33x
slower the Halo is not the training box either way."* **That premise died with
this handoff**, so the test became worth running.

### The confound: box GTT cannot attribute

`mem_info_gtt_used` is whole-box, and this box has GPU co-tenants — the
sovereign daemon and its `--compute-child` were measured holding **26.9 GB
resident** during a probe. `train_orpo_trl.py` now records both per step:
`gtt_gb` (box) and `proc_gtt_gb` (this pid, from `drm-resident-gtt` in
`/proc/<pid>/fdinfo/<fd>`, deduped on `drm-client-id` because every fd on the
render node reports identical totals). A gap between the columns is now visibly
a co-tenant rather than silently a leak.

**Retraction:** I first ran a 150-step probe and reported box GTT climbing
8.3 → 79.7 GB as a reproduced ratchet. It was the daemon's compute-child
loading a model mid-run. With attribution, that same config is flat.

### The A/B — a clean negative

150 steps each, seq 4096, accum 1, arms differing *only* in
`--empty-cache-every`. `max|B|` came out bit-identical (5.483e-03), so the arms
are genuinely comparable:

| arm | proc GTT median | p90 | peak | split-half | s/it |
|---|---|---|---|---|---|
| baseline | 4.65 GB | 6.0 | 42.64 | 4.56 → 4.84 | 5.24 |
| `--empty-cache-every 10` | 4.6 GB | 5.9 | 42.63 | — | 5.29 |

**The mitigation at `M0_PROBE_HALO.md:79` does nothing** — peak unchanged to
0.01 GB, +1% wall clock. Don't adopt it, and don't re-test it at this step count.

### What the trainer actually does

Flat at ~4.7 GB, with **7 spikes of 149 steps to 25–42.6 GB**, all in the second
half. They are **not** long sequences: spiking steps are marginally *faster*
(median 4.87 s vs 5.24 s), which also refutes M0's "ratchets as progressively
longer sequences appear."

That reframes the OOM candidate as **interaction, not accumulation**: a 42.6 GB
trainer spike on top of a ~35 GB resident daemon is 78 GB of 125, and box peak
hit 77.8 GB. If that is the mechanism, the fix is scheduling — don't train with
a big model resident in the daemon — and no allocator knob addresses it.

### What is still not settled

**M0's onset was ~1,600 micro-batches; this probe ran 150.** Thirteen times
short. A slow ratchet beyond 150 steps is *not* excluded, and nothing here
licenses starting a 2,334-iteration epoch. The definitive test is ~2.5 h at
accum 1 (1,700 steps × 5.24 s) with attribution on — worth asking before
running, since the compositor reserve guard has killed the graphical session
twice on this box.

## Assets

| What | Where |
|---|---|
| Gate run (PASS, 5 steps) | `/home/alexbryan/dev/train-env/runs/halo-gradcheck/` |
| Divergence run (DIVERGED, 25 steps) | `/home/alexbryan/dev/train-env/runs/ratchet-25/` |
| GTT trace, 25-step run | `/home/alexbryan/dev/train-env/runs/gtt_ratchet_probe.tsv` |
| Launcher (container-agnostic) | `/home/alexbryan/dev/train-env/launch_gradcheck.sh` |
| Gate, 3 verdicts, one shared rule | `scripts/check_adapter_trained.py` (`scan()`) |
| Trainer, now saving + self-gating | `scripts/train_orpo_trl.py` |

| A/B on the GTT mitigation | `/home/alexbryan/dev/train-env/runs/ab-{baseline,empty10}/` |

Notes: `0d3804bd` (the lane trains), `978a8583` (no second container needed),
`edbfabb8` (the gate's NaN blind spot), `c4851203` (seq 1024 truncates 92.5%),
`f1e96c88` (GTT is co-tenancy + `empty_cache` is a clean negative — supersedes
`5780c214` and `8de0d918`, both of which reasoned off unattributable box GTT).
