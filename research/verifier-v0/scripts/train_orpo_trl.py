#!/usr/bin/env python3
"""ORPO LoRA trainer on TRL — the Strix Halo (ROCm) half of VERIFIER_V0 §7 M0.

The M2 Max half of the M0 probe ran under `mlx_lm_lora`. This is the
counterpart so the wall-clock table in findings/M0_PROBE.md can name BOTH
boxes instead of extrapolating everything from the Mac.

Defaults mirror the Mac probe exactly (M0_PROBE.md:9):
    Qwen/Qwen3.5-0.8B, ORPO, LoRA r=32/alpha=64, lr 1e-4,
    batch 4 x grad-accum 8 (effective 32), seq 4096, 100 iters, data/orpo-probe.

Glassbox: every optimizer step emits wall-clock, loss, host RSS and GPU memory
to a JSONL sidecar, and the run ends by writing a summary JSON with the s/it
distribution. A timing claim you cannot re-derive from the log is not a
measurement.

Usage:
    train_orpo_trl.py --out runs/probe-halo-trl
    train_orpo_trl.py --out runs/probe-halo-unsloth --unsloth
"""

from __future__ import annotations

import argparse
import inspect
import json
import os
import platform
import statistics
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]  # research/verifier-v0
DEFAULT_DATA = REPO / "data" / "orpo-probe"

# The B-matrix rule lives in exactly one place. check_adapter_trained.py is the
# standalone auditor for arbitrary adapter dirs; this trainer applies the SAME
# rule to its own output, so a run can never again look healthy while having
# learned nothing (HALO_HANDOFF_2026-08-02.md §1).
sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_adapter_trained import DIVERGED, TRAINED, report, scan  # noqa: E402


# --------------------------------------------------------------------------
# observability helpers
# --------------------------------------------------------------------------

def host_rss_gb() -> float:
    """Resident set of THIS process, in GB. /proc is authoritative; psutil is
    an optional dependency we do not want to require."""
    try:
        with open(f"/proc/{os.getpid()}/status") as fh:
            for line in fh:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) / (1024 * 1024)
    except OSError:
        pass
    return float("nan")


def host_gtt_gb() -> float:
    """GTT held by the WHOLE BOX, from sysfs.

    `torch.cuda.max_memory_allocated()` cannot see GTT reserved by the HIP
    runtime outside PyTorch's allocator. On the M0 probe it read 29.34 GB flat
    while the box held 103 GB and the trainer was SIGKILLed
    (findings/M0_PROBE_HALO.md:72). Recording only torch's counter is how a run
    ratchets to death while its own log reports a steady 29 GB.

    This number is the box's, NOT this process's — see proc_gtt_gb.
    """
    try:
        with open("/sys/class/drm/card1/device/mem_info_gtt_used") as fh:
            return int(fh.read().strip()) / 1024**3
    except OSError:
        return float("nan")


def proc_gtt_gb(pid: int | None = None) -> float:
    """GTT resident for THIS process alone, attributed via drm fdinfo.

    WHY THIS EXISTS. The sysfs counter above is box-wide, and this box has GPU
    co-tenants: the sovereign daemon and its `--compute-child` hold renderD128
    open and were measured at 26.9 GB resident while a training probe ran.
    Attributing that to the trainer would turn a co-tenant's model load into a
    phantom "leak", and — worse — a real leak into a shrug about the daemon.
    M0's attribution was done by hand once (`/proc/*/fd`, one pid holding GPU
    fds); this makes it automatic and per-step.

    Dedupes on drm-client-id: a process opens the render node many times and
    every fd reports the SAME totals, so summing over fds inflates by the fd
    count.
    """
    pid = pid or os.getpid()
    seen: dict[str, int] = {}
    fddir = f"/proc/{pid}/fd"
    try:
        fds = os.listdir(fddir)
    except OSError:
        return float("nan")
    for fd in fds:
        try:
            if "renderD" not in os.readlink(f"{fddir}/{fd}"):
                continue
            client = None
            kib = 0
            with open(f"/proc/{pid}/fdinfo/{fd}") as fh:
                for line in fh:
                    if line.startswith("drm-client-id:"):
                        client = line.split()[-1]
                    elif line.startswith("drm-resident-gtt:"):
                        kib = int(line.split()[1])
            if client is not None:
                seen[client] = max(seen.get(client, 0), kib)
        except (OSError, ValueError, IndexError):
            continue
    if not seen:
        return float("nan")
    return sum(seen.values()) / 1024**2


def gpu_mem_gb(torch) -> tuple[float, float]:
    if not torch.cuda.is_available():
        return (float("nan"), float("nan"))
    return (
        torch.cuda.memory_allocated() / 1024**3,
        torch.cuda.max_memory_allocated() / 1024**3,
    )


VISION_HINTS = ("visual", "vision", "image", "vit", "patch_embed", "merger")


def language_linear_modules(model) -> tuple[list[str], dict[str, int]]:
    """Every nn.Linear on the LANGUAGE side, by full module name.

    Why not `target_modules="all-linear"`: Qwen3.5-0.8B is a
    `Qwen3_5ForConditionalGeneration` — it carries a vision tower and an MTP
    head that the Mac probe did not adapt (M0_PROBE.md counted 186 adapted
    modules across linear_attn + self_attn, language side only). "all-linear"
    would silently adapt the vision tower too, making the two probes different
    experiments wearing the same config.

    Full names are returned rather than leaf names because peft matches by
    suffix: leaf names like `q_proj` would re-admit the vision tower through
    the back door.
    """
    import torch.nn as nn

    names: list[str] = []
    for name, mod in model.named_modules():
        if not isinstance(mod, nn.Linear):
            continue
        low = name.lower()
        if any(h in low for h in VISION_HINTS):
            continue
        # lm_head is the output projection; MTP is a separate prediction head.
        # Neither is a decoder projection, and neither was adapted on the Mac.
        if low.endswith("lm_head") or ".mtp" in low or low.startswith("mtp"):
            continue
        names.append(name)

    breakdown: dict[str, int] = {}
    for n in names:
        leaf = n.split(".")[-1]
        breakdown[leaf] = breakdown.get(leaf, 0) + 1
    return names, breakdown


def load_base_model(args, torch, dtype):
    """Resolve the model class from the checkpoint rather than assuming one.

    `AutoModelForCausalLM` raises on a `*ForConditionalGeneration` config in
    some transformers versions; when it does, honour the architecture the
    checkpoint itself declares.
    """
    from transformers import AutoConfig, AutoModelForCausalLM

    cfg = AutoConfig.from_pretrained(args.model)
    arch = (getattr(cfg, "architectures", None) or ["<unstated>"])[0]
    print(f"checkpoint architecture: {arch} (model_type={cfg.model_type})")
    kwargs = dict(dtype=dtype, attn_implementation=args.attn)
    try:
        return AutoModelForCausalLM.from_pretrained(args.model, **kwargs), arch
    except (ValueError, KeyError) as exc:
        print(f"  AutoModelForCausalLM refused it ({type(exc).__name__}); "
              f"falling back to {arch} directly")
        import transformers as tf
        cls = getattr(tf, arch, None)
        if cls is None:
            raise RuntimeError(
                f"transformers {tf.__version__} does not export {arch}. "
                f"The installed transformers cannot load this checkpoint, so a "
                f"Halo number here would not be the same experiment as the Mac's."
            ) from exc
        return cls.from_pretrained(args.model, **kwargs), arch


def describe_environment(torch) -> dict:
    """Everything a reader needs to decide whether a later run is comparable."""
    env = {
        "host": platform.node(),
        "platform": platform.platform(),
        "python": sys.version.split()[0],
        "torch": torch.__version__,
        "torch_hip": getattr(torch.version, "hip", None),
        "torch_cuda": getattr(torch.version, "cuda", None),
        "container": None,
        "gpu": None,
        "gpu_count": torch.cuda.device_count() if torch.cuda.is_available() else 0,
        "hsa_override": os.environ.get("HSA_OVERRIDE_GFX_VERSION"),
    }
    try:
        with open("/run/.containerenv") as fh:
            for line in fh:
                if line.startswith("name="):
                    env["container"] = line.split("=", 1)[1].strip().strip('"')
    except OSError:
        pass
    if torch.cuda.is_available():
        env["gpu"] = torch.cuda.get_device_name(0)
        try:
            props = torch.cuda.get_device_properties(0)
            env["gpu_arch"] = getattr(props, "gcnArchName", None)
            env["gpu_total_mem_gb"] = round(props.total_memory / 1024**3, 2)
        except Exception:  # noqa: BLE001 - purely informational
            pass
    for mod in ("trl", "peft", "transformers", "datasets", "accelerate", "unsloth"):
        try:
            env[mod] = __import__(mod).__version__
        except Exception:  # noqa: BLE001
            env[mod] = None

    # Which gated-deltanet memory path will this run take? On gfx1151 that
    # single fact decides whether the run survives, and until now NOTHING
    # recorded it: the only trace in train.log was a transformers warning
    # string, so reconstructing a past run's path meant grepping for prose.
    #
    # transformers gates on `is_fast_path_available = all(...)` over four
    # symbols. With fla only, three of four resolve -> SEQUENTIAL path,
    # ~25 GB GTT, survives. Add causal-conv1d and all four resolve ->
    # CHUNKED path, ~100 GB GTT, killed by the host OOM killer.
    # See note e643e089. find_spec, not import: no side effects, no CUDA
    # extension load, no cost to a run that is about to be timed.
    import importlib.util as _ilu

    for mod in ("fla", "causal_conv1d", "triton"):
        try:
            env[mod] = "present" if _ilu.find_spec(mod) else None
        except Exception:  # noqa: BLE001
            env[mod] = None
    env["deltanet_path"] = (
        "chunked (INFEASIBLE on gfx1151 -- expect ~100 GB GTT and SIGKILL)"
        if env.get("causal_conv1d") and env.get("fla")
        else "sequential" if env.get("fla")
        else "eager-torch (no fla -- ~1.3x slower)"
    )
    return env


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", default="Qwen/Qwen3.5-0.8B")
    ap.add_argument("--data", type=Path, default=DEFAULT_DATA,
                    help="directory holding train.jsonl / valid.jsonl")
    ap.add_argument("--out", type=Path, required=True, help="run directory")
    ap.add_argument("--iters", type=int, default=100, help="optimizer steps")
    ap.add_argument("--batch-size", type=int, default=4)
    ap.add_argument("--grad-accum", type=int, default=8)
    ap.add_argument("--seq-len", type=int, default=4096)
    ap.add_argument("--lr", type=float, default=1e-4)
    ap.add_argument("--lora-r", type=int, default=32)
    ap.add_argument("--lora-alpha", type=int, default=64)
    ap.add_argument("--beta", type=float, default=0.1, help="ORPO lambda/beta")
    ap.add_argument("--unsloth", action="store_true",
                    help="load through unsloth.FastLanguageModel instead of plain HF")
    ap.add_argument("--grad-checkpointing", action="store_true",
                    help="trade speed for memory; OFF by default so the s/it is "
                         "comparable to the Mac probe, which did not use it")
    ap.add_argument("--attn", default="sdpa", choices=["sdpa", "eager", "flash_attention_2"])
    ap.add_argument("--dtype", default="bfloat16", choices=["bfloat16", "float16", "float32"])
    ap.add_argument("--seed", type=int, default=17, help="matches the data seed")
    ap.add_argument("--empty-cache-every", type=int, default=0, metavar="N",
                    help="call torch.cuda.empty_cache() every N optimizer steps "
                         "(0 = never). The untested GTT-ratchet mitigation from "
                         "M0_PROBE_HALO.md:79 — measure s/it against a baseline "
                         "run before adopting it, empty_cache is not free.")
    ap.add_argument("--gtt-limit-gb", type=float, default=95.0,
                    help="abort if BOX GTT exceeds this (default 95 of 125 GB, "
                         "leaving the compositor its reserve). M0 was SIGKILLed "
                         "at 100.7 GB and took the desktop session with it; a "
                         "clean stop keeps the adapter, the timings and the log.")
    ap.add_argument("--rss-limit-gb", type=float, default=40.0,
                    help="abort if this process exceeds this RSS (the Mac run's "
                         "tripwire; peak trainer RSS there was ~22 GB)")
    args = ap.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    steps_path = args.out / "steps.jsonl"
    summary_path = args.out / "summary.json"

    # -- imports are deferred so --help works without the stack installed ----
    import torch
    from datasets import load_dataset
    from peft import LoraConfig
    from transformers import AutoModelForCausalLM, AutoTokenizer, TrainerCallback
    # TRL moved ORPO under `trl.experimental` in 1.x (verified on trl 1.9.2,
    # where a top-level `from trl import ORPOConfig` raises ImportError and
    # helpfully suggests GRPOConfig instead). Try the classic path first so
    # this still runs against pre-1.0 installs.
    os.environ.setdefault("TRL_EXPERIMENTAL_SILENCE", "1")
    try:
        from trl import ORPOConfig, ORPOTrainer
        trl_orpo_path = "trl"
    except ImportError:
        from trl.experimental.orpo import ORPOConfig, ORPOTrainer
        trl_orpo_path = "trl.experimental.orpo"

    env = describe_environment(torch)
    env["trl_orpo_import_path"] = trl_orpo_path
    env["ld_preload"] = os.environ.get("LD_PRELOAD")
    print("=== environment ===")
    for k, v in env.items():
        print(f"  {k:18s} {v}")

    # gfx1151 guard. torch's bundled ROCm 7.0 HSA runtime segfaults on the
    # first GPU copy/kernel under ROCm 7.2.4 -- AFTER reporting the device
    # perfectly. Catch it here, where the message can name the fix, rather than
    # 40 seconds into training as an unexplained core dump.
    HSA_FIX = "/opt/rocm/lib/libhsa-runtime64.so.1"
    if (env.get("gpu_arch") == "gfx1151"
            and HSA_FIX not in (os.environ.get("LD_PRELOAD") or "")
            and os.path.exists(HSA_FIX)):
        print(f"\nFATAL: gfx1151 without the system HSA runtime preloaded.\n"
              f"Every GPU op will SIGSEGV once training starts, even though "
              f"torch reports the device correctly above.\n\n"
              f"    export LD_PRELOAD={HSA_FIX}\n\n"
              f"(HSA_OVERRIDE_GFX_VERSION and LD_LIBRARY_PATH do NOT fix this "
              f"-- see note b18dacf9.)", file=sys.stderr)
        return 2

    if not torch.cuda.is_available():
        print("\nFATAL: no GPU visible to torch. A CPU s/it number answers no "
              "question this probe was created to answer.\n"
              "Try: export HSA_OVERRIDE_GFX_VERSION=11.0.0", file=sys.stderr)
        return 2

    torch.manual_seed(args.seed)
    dtype = getattr(torch, args.dtype)

    # -- data ---------------------------------------------------------------
    train_file = args.data / "train.jsonl"
    valid_file = args.data / "valid.jsonl"
    if not train_file.exists():
        print(f"FATAL: no train.jsonl under {args.data}", file=sys.stderr)
        return 2
    ds = load_dataset("json", data_files={"train": str(train_file)})["train"]
    missing = {"prompt", "chosen", "rejected"} - set(ds.column_names)
    if missing:
        print(f"FATAL: dataset missing columns {missing}", file=sys.stderr)
        return 2
    print(f"\ntrain rows: {len(ds)}  ({train_file})")

    # -- model --------------------------------------------------------------
    t_load = time.monotonic()
    peft_config = None
    arch = None
    targets: list[str] = []
    breakdown: dict[str, int] = {}

    if args.unsloth:
        from unsloth import FastLanguageModel
        model, tokenizer = FastLanguageModel.from_pretrained(
            model_name=args.model,
            max_seq_length=args.seq_len,
            dtype=dtype,
            load_in_4bit=False,
        )
        model = FastLanguageModel.get_peft_model(
            model,
            r=args.lora_r,
            lora_alpha=args.lora_alpha,
            lora_dropout=0.0,
            bias="none",
            use_gradient_checkpointing="unsloth" if args.grad_checkpointing else False,
            random_state=args.seed,
            target_modules=["q_proj", "k_proj", "v_proj", "o_proj",
                            "gate_proj", "up_proj", "down_proj"],
        )
        peft_config = None  # unsloth already wrapped it
    else:
        tokenizer = AutoTokenizer.from_pretrained(args.model)
        model, arch = load_base_model(args, torch, dtype)
        model.to("cuda")

        targets, breakdown = language_linear_modules(model)
        if not targets:
            print("FATAL: found no language-side nn.Linear modules to adapt",
                  file=sys.stderr)
            return 2
        print(f"adapting {len(targets)} language-side Linear modules "
              f"(Mac probe: 186). By leaf name:")
        for leaf, count in sorted(breakdown.items(), key=lambda kv: -kv[1]):
            print(f"    {leaf:24s} {count}")
        peft_config = LoraConfig(
            r=args.lora_r,
            lora_alpha=args.lora_alpha,
            lora_dropout=0.0,
            bias="none",
            task_type="CAUSAL_LM",
            target_modules=targets,
        )

    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token
    load_s = time.monotonic() - t_load
    print(f"model loaded in {load_s:.1f}s")

    # -- config -------------------------------------------------------------
    cfg_kwargs = dict(
        output_dir=str(args.out / "hf"),
        max_steps=args.iters,
        per_device_train_batch_size=args.batch_size,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        beta=args.beta,
        max_length=args.seq_len,
        max_prompt_length=args.seq_len // 2,
        logging_steps=1,
        save_strategy="no",
        report_to=[],
        disable_tqdm=True,  # the bars flood a captured log with 100+ KB of \r frames
        seed=args.seed,
        bf16=(args.dtype == "bfloat16"),
        fp16=(args.dtype == "float16"),
        gradient_checkpointing=args.grad_checkpointing,
        remove_unused_columns=False,
    )
    # TRL has churned on this field name; keep both spellings working.
    sig = inspect.signature(ORPOConfig.__init__).parameters
    cfg_kwargs = {k: v for k, v in cfg_kwargs.items() if k in sig}
    cfg = ORPOConfig(**cfg_kwargs)

    # -- per-step instrumentation ------------------------------------------
    class StepTimer(TrainerCallback):
        def __init__(self) -> None:
            self.last = None
            self.durations: list[float] = []
            self.fh = open(steps_path, "w")
            self.t0 = time.monotonic()

        def on_step_end(self, targs, state, control, **kw):  # noqa: ANN001
            now = time.monotonic()
            if self.last is not None:
                self.durations.append(now - self.last)
            self.last = now
            # The mitigation named as untested in M0_PROBE_HALO.md:79. Runs
            # BEFORE the memory sample so the sample reports the post-reclaim
            # figure — otherwise the trace measures the thing we just released.
            if args.empty_cache_every and state.global_step % args.empty_cache_every == 0:
                torch.cuda.empty_cache()

            alloc, peak = gpu_mem_gb(torch)
            rss = host_rss_gb()
            rec = {
                "step": state.global_step,
                "elapsed_s": round(now - self.t0, 3),
                "step_s": round(self.durations[-1], 3) if self.durations else None,
                "loss": (state.log_history[-1].get("loss")
                         if state.log_history else None),
                "rss_gb": round(rss, 2),
                "gpu_alloc_gb": round(alloc, 2),
                "gpu_peak_gb": round(peak, 2),
                "gtt_gb": round(host_gtt_gb(), 2),      # whole box
                "proc_gtt_gb": round(proc_gtt_gb(), 2),  # this trainer alone
            }
            self.fh.write(json.dumps(rec) + "\n")
            self.fh.flush()
            if state.global_step % 5 == 0 or state.global_step <= 3:
                s = rec["step_s"]
                print(f"  step {rec['step']:4d}  {s if s is None else f'{s:6.2f}'}s/it"
                      f"  loss={rec['loss']}  rss={rec['rss_gb']}GB"
                      f"  gpu={rec['gpu_alloc_gb']}/{rec['gpu_peak_gb']}GB", flush=True)
            if rss == rss and rss > args.rss_limit_gb:
                print(f"\nABORT: RSS {rss:.1f}GB exceeded --rss-limit-gb "
                      f"{args.rss_limit_gb}. This tripwire exists because a "
                      f"four-job pileup locked the machine on 2026-07-29.",
                      file=sys.stderr)
                control.should_training_stop = True

            # THE TRIPWIRE THAT MATTERS ON THIS BOX. The RSS one above cannot
            # fire on the failure we actually have: at M0's death host RSS was
            # under 1 GB while GTT stood at 100.7 GB. Unified memory means GTT
            # pressure is what SIGKILLs the trainer and what has twice taken the
            # desktop compositor with it, so the guard has to watch GTT — and
            # BOX GTT, not this process's, because a co-tenant's model load
            # counts against the same 125 GB.
            box = rec["gtt_gb"]
            if box == box and box > args.gtt_limit_gb:
                print(f"\nABORT: box GTT {box:.1f}GB exceeded --gtt-limit-gb "
                      f"{args.gtt_limit_gb} (this process holds "
                      f"{rec['proc_gtt_gb']:.1f}GB of it). Stopping cleanly so "
                      f"the adapter and timings survive — an OOM SIGKILL would "
                      f"take both, and the graphical session with them.",
                      file=sys.stderr)
                control.should_training_stop = True
            return control

    timer = StepTimer()

    trainer_kwargs = dict(
        model=model,
        args=cfg,
        train_dataset=ds,
        callbacks=[timer],
    )
    if peft_config is not None:
        trainer_kwargs["peft_config"] = peft_config
    tsig = inspect.signature(ORPOTrainer.__init__).parameters
    trainer_kwargs["processing_class" if "processing_class" in tsig else "tokenizer"] = tokenizer

    trainer = ORPOTrainer(**trainer_kwargs)

    adapted = [n for n, _ in trainer.model.named_parameters() if "lora_" in n]
    print(f"LoRA tensors: {len(adapted)}  "
          f"(Mac probe adapted 186 modules across linear_attn + self_attn)")

    # -- train --------------------------------------------------------------
    print(f"\n=== training: {args.iters} steps, effective batch "
          f"{args.batch_size * args.grad_accum}, seq {args.seq_len} ===", flush=True)
    t_train = time.monotonic()
    status = "ok"
    try:
        trainer.train()
    except KeyboardInterrupt:
        status = "interrupted"
    except Exception as exc:  # noqa: BLE001 - we want the partial timings
        status = f"error: {type(exc).__name__}: {exc}"
        print(f"\nTRAINING FAILED: {status}", file=sys.stderr)
    train_s = time.monotonic() - t_train
    timer.fh.close()

    # -- adapter save + gradient gate ---------------------------------------
    # UNCONDITIONAL, and it runs even when `status != "ok"`: a run that died at
    # step 63 still answers "did gradients ever reach the weights?", and that is
    # the question four days of Mac runs never got asked. An adapter is ~80 MB;
    # the throughput cost is one save. See HALO_HANDOFF_2026-08-02.md §4.
    adapter_dir = args.out / "adapter"
    try:
        trainer.model.save_pretrained(str(adapter_dir))
        verdict = scan(adapter_dir)
    except Exception as exc:  # noqa: BLE001 - never lose the timings over a save
        verdict = {"saved": False, "verdict": "unusable", "trained": None,
                   "error": f"{type(exc).__name__}: {exc}"}
        print(f"\nADAPTER SAVE FAILED: {verdict['error']}", file=sys.stderr)

    # -- summary ------------------------------------------------------------
    step_recs = [json.loads(l) for l in steps_path.read_text().splitlines()]

    def _series(key: str) -> list[float]:
        return [v for v in (r.get(key) for r in step_recs)
                if v is not None and v == v]  # drop nulls and NaN

    gtt_series = _series("proc_gtt_gb") or _series("gtt_gb")
    box_series = _series("gtt_gb")
    d = timer.durations
    # The first optimizer step carries compile/warmup cost; report both so the
    # steady-state number cannot be quietly confused with the overall one.
    steady = d[1:] if len(d) > 1 else d
    summary = {
        "status": status,
        "run": str(args.out),
        "backend": "unsloth" if args.unsloth else "trl-vanilla",
        "environment": env,
        "config": {
            "model": args.model,
            "data": str(args.data),
            "train_rows": len(ds),
            "iters_requested": args.iters,
            "steps_timed": len(d),
            "batch_size": args.batch_size,
            "grad_accum": args.grad_accum,
            "effective_batch": args.batch_size * args.grad_accum,
            "seq_len": args.seq_len,
            "lr": args.lr,
            "lora_r": args.lora_r,
            "lora_alpha": args.lora_alpha,
            "beta": args.beta,
            "dtype": args.dtype,
            "attn": args.attn,
            "grad_checkpointing": args.grad_checkpointing,
            "architecture": arch,
            "lora_tensors": len(adapted),
            "adapted_modules": len(targets),
            "adapted_by_leaf": breakdown,
        },
        "timing": {
            "model_load_s": round(load_s, 1),
            "train_wall_s": round(train_s, 1),
            "s_per_it_overall": round(train_s / max(len(d), 1), 2),
            "s_per_it_mean_steady": round(statistics.mean(steady), 2) if steady else None,
            "s_per_it_median_steady": round(statistics.median(steady), 2) if steady else None,
            "s_per_it_min": round(min(d), 2) if d else None,
            "s_per_it_max": round(max(d), 2) if d else None,
            "first_step_s": round(d[0], 2) if d else None,
        },
        "memory": {
            "host_rss_peak_gb": round(max(
                (r["rss_gb"] for r in step_recs), default=float("nan")), 2),
            "gpu_peak_gb": round(gpu_mem_gb(torch)[1], 2),
            # GTT first/last/peak/slope, because the FAILURE MODE on this box is
            # a SLOPE, not a level. A single peak cannot distinguish "held 25 GB
            # steadily" from "ratcheted 25 -> 103 and got SIGKILLed", and those
            # are the two outcomes we actually need to tell apart. These are
            # THIS PROCESS's GTT; box_gtt_peak_gb is the whole machine, and a
            # gap between them is a co-tenant, not a leak.
            "gtt_first_gb": gtt_series[0] if gtt_series else None,
            "gtt_last_gb": gtt_series[-1] if gtt_series else None,
            "gtt_peak_gb": max(gtt_series) if gtt_series else None,
            "gtt_growth_mb_per_step": (
                round((gtt_series[-1] - gtt_series[0]) * 1024 / (len(gtt_series) - 1), 1)
                if len(gtt_series) > 1 else None),
            "box_gtt_peak_gb": max(box_series) if box_series else None,
            "empty_cache_every": args.empty_cache_every,
        },
        "adapter": verdict,
    }
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    print("\n=== summary ===")
    print(json.dumps(summary["timing"], indent=2))
    print(json.dumps(summary["memory"], indent=2))
    print(json.dumps(summary["adapter"], indent=2))
    print(f"\nwrote {summary_path}\nwrote {steps_path}")

    # The gate, stated where nobody can miss it. A timing number from a run that
    # did not train is not a slower measurement — it is not a measurement.
    if verdict.get("verdict") == TRAINED:
        print(f"\nGATE PASS -- adapter TRAINED, max|B| {verdict['max_abs_b']:.6e} "
              f"({verdict['b_nonzero']}/{verdict['lora_b_tensors']} B tensors nonzero)")
    else:
        print("\nGATE FAIL\n" + report(verdict), file=sys.stderr)
        # Distinct codes: a diverged run needs an LR change, a not-trained run
        # needs a framework fix. A caller must be able to tell them apart.
        return 4 if verdict.get("verdict") == DIVERGED else 3

    if status != "ok":
        return 1

    # Sizing extrapolation, stated in the same shape as M0_PROBE.md's table.
    sit = summary["timing"]["s_per_it_median_steady"] or summary["timing"]["s_per_it_overall"]
    eff = args.batch_size * args.grad_accum
    # Real epoch sizes, from the manifests — not estimates. Stream A train is
    # data/orpo-76k (74,674 after the 1k/1k valid/test split); A+B train is
    # data/orpo-ab (93,693 = 76,674 A + 19,019 B, 19.9% B, built 2026-08-01).
    for name, rows in (("Stream A (orpo-76k train)", 74674),
                       ("A+B (orpo-ab train)", 93693)):
        iters = rows / eff
        print(f"  {name}: {iters:,.0f} iters/epoch x {sit}s = "
              f"{iters * sit / 3600:.1f} h/epoch")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
