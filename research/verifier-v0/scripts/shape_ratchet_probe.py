#!/usr/bin/env python3
"""Does SEQUENCE-SHAPE VARIETY cause the GTT ratchet? One variable, two arms.

    shape_ratchet_probe.py --out <dir> --mode vary|fixed [--iters 50]

WHY THIS EXISTS. `M2_HALO_GRADCHECK.md` reports a "GTT ratchet" that caps a
training process at ~118 optimizer steps, extrapolated to ~250 GB for a full
400-step arm. Re-reading the traces on 2026-08-03 shows that extrapolation is
built on a rising MEDIAN of a BIMODAL variable, and three facts do not fit a
leak:

  * `max_memory_allocated` is PINNED at 32.56 GB from step 2 to step 118 in
    runs/mix-A. Torch's peak demand never grows.
  * The proc-GTT FLOOR never rises. Step 113 reads 5.7 GB; step 118 reads
    101.3 GB. A leak raises the floor; this does not.
  * In runs/ab-baseline (accum 1) torch's peak stops growing at step 62
    (23.88 GB), yet every GTT spike lands at step 93 or later and reaches
    42.6 GB -- 1.8x torch's own all-time peak. The excess appears WITHOUT
    torch allocating anything new.

`proc_gtt_gb` reads `drm-resident-gtt`, which counts pages RESIDENT right now.
The driver evicts, so it is a noisy sample of an oscillating quantity, not a
high-water mark. What actually rises over a run is the FREQUENCY of high
samples -- consistent with torch's RESERVE growing while its ALLOCATION does
not, i.e. allocator segments accumulating because each new record sequence
length needs a segment no existing one can serve.

THE COLUMN NOBODY LOGGED. `train_orpo_trl.py:128` records
`memory_allocated` and `max_memory_allocated`. It never records
`memory_reserved()`. That is precisely the number that separates the two live
hypotheses, so every trace we own is blind to the mechanism.

WHAT THIS PROBE DOES. Same model, same dtype, same LoRA targets, same
gradient checkpointing as the real trainer. Forward + backward + step, N times.
The ONLY difference between arms is the sequence length fed each iteration:

    --mode vary    lengths drawn from a seeded spread (new record highs keep
                   arriving, as they do in the real data stream)
    --mode fixed   every iteration at the SAME length (the arm's own max), so
                   total compute is >= the vary arm and cannot explain a
                   smaller footprint

DECISION RULE, STATED BEFORE THE RUN (do not renegotiate it afterwards):

  A. `reserved` climbs in vary and is flat in fixed
     -> in-torch fragmentation driven by shape variety. CONFIRMED.
        Fix: bucket/pad sequence lengths (`group_by_length`, or pad to a
        multiple). Single-process 400 steps becomes possible.
  B. `reserved` flat in BOTH, but `proc_gtt - reserved` climbs
     -> growth is OUTSIDE torch's allocator (HIP runtime, fla/Triton kernel
        cache keyed on shape). Shape bucketing may still fix it, but no torch
        allocator knob will. REFUTES A.
  C. Neither climbs in either arm
     -> shape variety is not the mechanism. This probe is negative; fall back
        to an instrumented real-trainer run.

The probe is synthetic: random token ids, no real data, no ORPO loss. It is an
instrument for MEMORY SHAPE BEHAVIOUR only. It cannot and does not say anything
about training quality.
"""
import argparse
import json
import os
import pathlib
import random
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))


def proc_gtt_detail() -> tuple[float, int, float]:
    """GTT resident for THIS process, via drm fdinfo -> (summed, clients, max).

    The trainer's `proc_gtt_gb` returns the SUM over distinct drm-client-ids.
    That is only correct if each client id owns a disjoint set of buffers. If a
    process accumulates render-node clients that each report an overlapping or
    process-wide total, the sum over-counts by roughly the client count -- and
    the reported number climbs for reasons that have nothing to do with memory.

    So this returns the client COUNT and the largest SINGLE client alongside
    the sum, and the probe records all three. If sum >> max and clients > 1,
    the metric is inflating and every GTT figure derived from it is suspect.
    """
    pid = os.getpid()
    seen: dict[str, int] = {}
    fddir = f"/proc/{pid}/fd"
    try:
        fds = os.listdir(fddir)
    except OSError:
        return (float("nan"), 0, float("nan"))
    for fd in fds:
        try:
            if "renderD" not in os.readlink(f"{fddir}/{fd}"):
                continue
            client, kib = None, 0
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
        return (float("nan"), 0, float("nan"))
    return (sum(seen.values()) / 1024**2, len(seen),
            max(seen.values()) / 1024**2)


def proc_gtt_gb() -> float:
    return proc_gtt_detail()[0]


def box_gtt_gb() -> float:
    try:
        with open("/sys/class/drm/card1/device/mem_info_gtt_used") as fh:
            return int(fh.read().strip()) / 1024**3
    except OSError:
        return float("nan")


def lengths_for(mode: str, iters: int, seed: int, lo: int, hi: int) -> list[int]:
    """Length schedule per arm. Both arms see the same TOTAL token count budget
    at minimum -- fixed runs at the vary arm's MAXIMUM, so if fixed uses less
    memory it cannot be because it did less work.
    """
    rng = random.Random(seed)
    # Multiples of 64: real batches pad to something, and quantising removes
    # "every length is unique" as a trivial confound. Vary still sees ~50
    # distinct shapes; fixed sees exactly 1.
    vary = [rng.randrange(lo, hi + 1, 64) for _ in range(iters)]
    if mode == "vary":
        return vary
    return [max(vary)] * iters


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model",
                    default="/home/alexbryan/dev/train-env/models/Qwen3.5-0.8B")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--mode", choices=["vary", "fixed"], required=True)
    ap.add_argument("--iters", type=int, default=50)
    ap.add_argument("--batch", type=int, default=2,
                    help="2 matches ORPO's chosen+rejected concat")
    ap.add_argument("--min-len", type=int, default=768)
    ap.add_argument("--max-len", type=int, default=4096)
    ap.add_argument("--seed", type=int, default=17)
    ap.add_argument("--lora-r", type=int, default=32)
    ap.add_argument("--lora-alpha", type=int, default=64)
    ap.add_argument("--attn", default="sdpa")
    ap.add_argument("--gtt-limit-gb", type=float, default=95.0,
                    help="hard stop; the compositor reserve guard has killed "
                         "the graphical session on this box twice")
    args = ap.parse_args(argv[1:])
    args.out.mkdir(parents=True, exist_ok=True)

    import torch
    from peft import LoraConfig, get_peft_model
    from train_orpo_trl import language_linear_modules, load_base_model

    print(f"mode={args.mode} iters={args.iters} batch={args.batch} "
          f"len=[{args.min_len},{args.max_len}] seed={args.seed}")
    print(f"box GTT at launch: {box_gtt_gb():.2f} GB")

    t0 = time.monotonic()
    model, arch = load_base_model(args, torch, torch.bfloat16)
    model.to("cuda")
    targets, _ = language_linear_modules(model)
    model = get_peft_model(model, LoraConfig(
        r=args.lora_r, lora_alpha=args.lora_alpha, lora_dropout=0.0,
        bias="none", task_type="CAUSAL_LM", target_modules=targets))
    model.gradient_checkpointing_enable()
    model.enable_input_require_grads()
    model.train()
    opt = torch.optim.AdamW(
        [p for p in model.parameters() if p.requires_grad], lr=1e-4)
    print(f"model loaded in {time.monotonic() - t0:.1f}s, "
          f"{len(targets)} adapted modules, arch={arch}")

    vocab = int(model.config.vocab_size)
    schedule = lengths_for(args.mode, args.iters, args.seed,
                           args.min_len, args.max_len)
    print(f"distinct sequence lengths this arm: {len(set(schedule))}")

    steps_path = args.out / "steps.jsonl"
    gen = torch.Generator(device="cpu").manual_seed(args.seed)
    seen_shapes: set[int] = set()
    stopped = None

    with open(steps_path, "w") as fh:
        for i, seq in enumerate(schedule, 1):
            t = time.monotonic()
            ids = torch.randint(0, vocab, (args.batch, seq), generator=gen,
                                dtype=torch.long).to("cuda")
            out = model(input_ids=ids, labels=ids)
            out.loss.backward()
            opt.step()
            opt.zero_grad(set_to_none=True)
            torch.cuda.synchronize()
            seen_shapes.add(seq)

            st = torch.cuda.memory_stats()
            gtt_sum, gtt_clients, gtt_max = proc_gtt_detail()
            rec = {
                "iter": i,
                "seq_len": seq,
                "distinct_shapes_seen": len(seen_shapes),
                "step_s": round(time.monotonic() - t, 3),
                "loss": round(float(out.loss), 4),
                # the two the trainer already had
                "alloc_gb": round(st["allocated_bytes.all.current"] / 1024**3, 3),
                "alloc_peak_gb": round(st["allocated_bytes.all.peak"] / 1024**3, 3),
                # THE COLUMN NOBODY LOGGED
                "reserved_gb": round(st["reserved_bytes.all.current"] / 1024**3, 3),
                "reserved_peak_gb": round(st["reserved_bytes.all.peak"] / 1024**3, 3),
                "segments": st["segment.all.current"],
                "alloc_retries": st["num_alloc_retries"],
                "inactive_split_gb": round(
                    st["inactive_split_bytes.all.current"] / 1024**3, 3),
                # the driver's view
                "proc_gtt_gb": round(gtt_sum, 2),
                "gtt_clients": gtt_clients,
                "gtt_max_client_gb": round(gtt_max, 2),
                "box_gtt_gb": round(box_gtt_gb(), 2),
            }
            # out-of-torch residency: what the driver holds that torch does not
            # claim to have reserved. This is the B-hypothesis discriminator.
            rec["gtt_minus_reserved_gb"] = round(
                rec["proc_gtt_gb"] - rec["reserved_gb"], 2)
            fh.write(json.dumps(rec) + "\n")
            fh.flush()
            print(f"  {i:>3} seq {seq:>5} shapes {len(seen_shapes):>3} "
                  f"resv {rec['reserved_gb']:>7.2f} segs {rec['segments']:>5} "
                  f"gtt {rec['proc_gtt_gb']:>7.2f} "
                  f"(clients {rec['gtt_clients']}, max {rec['gtt_max_client_gb']:>6.2f}, "
                  f"gtt-resv {rec['gtt_minus_reserved_gb']:>7.2f}) "
                  f"{rec['step_s']:>6.2f}s", flush=True)

            if rec["proc_gtt_gb"] >= args.gtt_limit_gb:
                stopped = f"gtt tripwire at {rec['proc_gtt_gb']:.1f} GB, iter {i}"
                print(f"STOPPING: {stopped}", flush=True)
                break

    rows = [json.loads(l) for l in open(steps_path)]
    first, last = rows[0], rows[-1]
    summary = {
        "mode": args.mode, "iters_run": len(rows), "stopped": stopped,
        "distinct_shapes": last["distinct_shapes_seen"],
        "reserved_first": first["reserved_gb"], "reserved_last": last["reserved_gb"],
        "reserved_peak": max(r["reserved_peak_gb"] for r in rows),
        "alloc_peak": max(r["alloc_peak_gb"] for r in rows),
        "segments_first": first["segments"], "segments_last": last["segments"],
        "alloc_retries_last": last["alloc_retries"],
        "proc_gtt_min": min(r["proc_gtt_gb"] for r in rows),
        "proc_gtt_max": max(r["proc_gtt_gb"] for r in rows),
        "gtt_minus_reserved_max": max(r["gtt_minus_reserved_gb"] for r in rows),
        "gtt_clients_first": first["gtt_clients"],
        "gtt_clients_last": last["gtt_clients"],
        "gtt_clients_max": max(r["gtt_clients"] for r in rows),
        "gtt_max_single_client": max(r["gtt_max_client_gb"] for r in rows),
        "total_tokens": sum(r["seq_len"] for r in rows) * args.batch,
        "wall_s": round(time.monotonic() - t0, 1),
    }
    (args.out / "summary.json").write_text(json.dumps(summary, indent=2))
    print("\n" + json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
