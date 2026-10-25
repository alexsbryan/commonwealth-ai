#!/usr/bin/env python3
"""The planner's own cut, mirrored from `sovereign_inference::rpc_warm_cache`
so the `-ot` D4 puts on llama-server's command line IS the crate's plan.

Mirrored, not borrowed: `plan_shards_weighted` (byte-mass arm, plan.rs:77-147)
is a pure function of the block-byte table and the device weights, and the
block table is the GGUF's tensor table — the same `tensor_sizes` the crate
reads. A mirror is a second implementation of one rule, which ARCH 8 forbids in
shipped code; this is a spike script, it lives in the bench lane beside
`dist-spike.sh`, and the test evidence for the crate stays the crate's own
tests. What the mirror buys: D4 can run the plan against the LIVE peer memory
without linking the daemon.

Subcommands:
  layers <gguf>                  header block count
  mass <gguf> <n_layer>          per-block bytes, head bytes (for the log)
  split <n_layer> <peer_gib> <head_gib>
                                 block counts, peer first (upstream's device
                                 order: RPC devices at the front)
  ot <counts> <peer-endpoint> <n_layer>
                                 the `-ot` string, each buffer named
                                 RPC<index within that server>[host:port]
"""
import struct
import sys

# (id, block_elems, bytes_per_block) — rpc_warm_cache/ggml_type.rs:10-30.
TYPE_SIZES = {
    0: (1, 4), 1: (1, 2), 2: (32, 18), 3: (32, 20), 6: (32, 22), 7: (32, 24),
    8: (32, 34), 9: (32, 36), 10: (256, 84), 11: (256, 110), 12: (256, 144),
    13: (256, 176), 14: (256, 210), 15: (256, 292), 16: (256, 66),
    17: (256, 74), 18: (256, 98), 19: (256, 50), 20: (32, 18), 21: (256, 110),
    22: (256, 82), 23: (256, 136), 24: (256, 36), 25: (256, 68), 26: (256, 56),
    27: (256, 56), 28: (256, 44), 29: (256, 112), 30: (1, 2), 34: (1, 2),
    35: (1, 4), 39: (32, 17),
}
QUANT = 4 * 1024**3  # plan.rs:35 quantize_vram bucket


def read_gguf(path):
    """(metadata dict of the keys we need, [(name, nbytes)])."""
    f = open(path, "rb")
    if f.read(4) != b"GGUF":
        raise SystemExit(f"{path}: not a GGUF")
    _version = struct.unpack("<I", f.read(4))[0]
    n_tensors = struct.unpack("<Q", f.read(8))[0]
    n_kv = struct.unpack("<Q", f.read(8))[0]

    def rstr():
        n = struct.unpack("<Q", f.read(8))[0]
        return f.read(n).decode("utf-8", "replace")

    def skip(t):
        fixed = {0: 1, 1: 1, 2: 2, 3: 2, 4: 4, 5: 4, 6: 4, 7: 1, 10: 8, 11: 8, 12: 8}
        if t in fixed:
            f.read(fixed[t])
            return
        if t == 8:
            f.read(struct.unpack("<Q", f.read(8))[0])
            return
        if t == 9:
            et = struct.unpack("<I", f.read(4))[0]
            for _ in range(struct.unpack("<Q", f.read(8))[0]):
                skip(et)
            return
        raise SystemExit(f"gguf: metadata value type {t} unhandled")

    meta = {}
    for _ in range(n_kv):
        key = rstr()
        t = struct.unpack("<I", f.read(4))[0]
        if t == 4 and key.endswith(".block_count"):
            meta["n_layer"] = struct.unpack("<I", f.read(4))[0]
        elif t == 8 and key == "general.architecture":
            meta["arch"] = rstr()
        else:
            skip(t)

    tensors = []
    for _ in range(n_tensors):
        name = rstr()
        n_dims = struct.unpack("<I", f.read(4))[0]
        ne = 1
        for _ in range(n_dims):
            ne *= struct.unpack("<Q", f.read(8))[0]
        ty = struct.unpack("<I", f.read(4))[0]
        f.read(8)  # offset; the sizes below are what the planner charges
        blck, size = TYPE_SIZES.get(ty, (1, 4))
        tensors.append((name, size * ne // blck))
    f.close()
    return meta, tensors


def layer_of(name):
    if not name.startswith("blk."):
        return None
    head = name[4:].split(".", 1)[0]
    return int(head) if head.isdigit() else None


def mass(tensors, n_layer):
    """plan.rs:325 model_mass_from_sizes — blocks, head, embedding, globals."""
    blocks = [0] * n_layer
    head = embd = other = 0
    for name, nbytes in tensors:
        layer = layer_of(name)
        if layer is not None and layer < n_layer:
            blocks[layer] += nbytes
        elif name == "output.weight":
            head += nbytes
        elif "token_embd" in name:
            embd += nbytes
        else:
            other += nbytes
    return blocks, head, embd, other


def quantize(b):
    if b == 0:
        return 0
    return max(1, (b + QUANT // 2) // QUANT) * QUANT


def split(n_layer, weights_bytes, block_bytes, head_bytes):
    """plan_shards_weighted's byte-mass arm (plan.rs:77-147), as counts.

    `weights_bytes` is each device's advertised free memory, peer first. The
    host (last device) also carries the head.
    """
    n = len(weights_bytes)
    q = [quantize(b) for b in weights_bytes]
    wsum = float(sum(q)) or 1.0
    w = [x / wsum for x in q]
    total_block = sum(block_bytes)
    total_mass = float(total_block) + float(head_bytes)
    head_dev = max((d for d in range(n) if w[d] > 0.0), default=None)
    tgt = [wd * total_mass for wd in w]
    if head_dev is not None:
        tgt[head_dev] = max(0.0, tgt[head_dev] - float(head_bytes))
    prefix = [0]
    for b in block_bytes:
        prefix.append(prefix[-1] + b)
    counts = [0] * n
    cum = 0.0
    prev = 0
    for d in range(n):
        cum += tgt[d]
        if d == n - 1:
            cut = n_layer
        else:
            cut = max(closest_boundary(prefix, cum, prev, n_layer), prev)
        counts[d] = cut - prev
        prev = cut
    return counts


def closest_boundary(prefix, target, lo, hi):
    best, best_d, b = lo, abs(prefix[lo] - target), lo + 1
    while b <= hi:
        cur = prefix[b]
        d = abs(cur - target)
        if d < best_d:
            best, best_d = b, d
        elif cur >= target:
            break
        b += 1
    return best


def main(argv):
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    cmd = argv[1]
    if cmd == "layers":
        meta, _ = read_gguf(argv[2])
        print(meta.get("n_layer") or 0)
        return 0
    if cmd == "mass":
        meta, tensors = read_gguf(argv[2])
        n_layer = int(argv[3]) if len(argv) > 3 else meta.get("n_layer", 0)
        blocks, head, embd, other = mass(tensors, n_layer)
        gib = 2**30
        print(f"arch {meta.get('arch')} blocks {n_layer}")
        print(f"block mass {sum(blocks)/gib:.2f} GiB, head {head/gib:.2f} GiB, "
              f"embd {embd/gib:.2f} GiB (host), other {other/gib:.2f} GiB (host)")
        if blocks:
            lo, hi = min(blocks), max(blocks)
            print(f"blocks range {lo/gib:.3f}..{hi/gib:.3f} GiB ({hi/max(lo,1):.1f}x) "
                  "— count-proportional is not byte-proportional on this stack"
                  if hi > lo * 1.05 else "blocks even — count split is byte split")
        return 0
    if cmd == "plan":
        # plan <gguf> <peer_gib> <head_gib>
        meta, tensors = read_gguf(argv[2])
        n_layer = meta.get("n_layer", 0)
        gib = 2**30
        weights = [int(float(x) * gib) for x in argv[3:5]]
        blocks, head, _, _ = mass(tensors, n_layer)
        counts = split(n_layer, weights, blocks, head)
        print(" ".join(str(c) for c in counts))
        return 0
    if cmd == "dts":
        # The -ts the planner must carry BESIDE its -ot: `dev_layer_tensor_split`
        # (plan.rs:612). llama.cpp derives its per-layer device map from
        # `tensor_split` alone (llama-model.cpp:1466, `upper_bound(splits, …)`);
        # `-ot` places the WEIGHTS and never touches that map, so an `-ot` with
        # no matching `-ts` leaves a layer straddling the cut — and on this
        # hybrid (Gated DeltaNet) that disables the fused GDN kernel globally
        # and multiplies wiring. The halves are cuts at `last + 0.5`, the
        # output-holder's extending to `n_layer + 1`, over `n_layer + 1` units.
        peer_blocks = int(argv[2])
        n_layer = int(argv[3])
        out = []
        prev = 0.0
        for d in range(2):
            last = peer_blocks - 1 if d == 0 else n_layer - 1
            cut = float(n_layer + 1) if d == 1 else float(last) + 0.5
            out.append(cut - prev)
            prev = cut
        print(",".join(f"{x:.4f}" for x in out))
        return 0
    if cmd == "ot":
        # ot <peer-blocks> <n_layer> <peer-endpoint>
        # The peer is device 0 of the two (RPC devices come first), so its
        # buffer is RPC0[<endpoint>] — the SERVER-scoped index, which is what
        # ggml_backend_rpc_buffer_type names (ggml-rpc.cpp:774). The head keeps
        # its own device and needs no override: llama.cpp's tensor_split
        # already puts the rest there when we hand it none, and the blocks
        # named here are exactly the peer's.
        peer_blocks = int(argv[2])
        n_layer = int(argv[3])
        peer = argv[4]
        if not 0 < peer_blocks <= n_layer:
            raise SystemExit(f"ot: {peer_blocks} blocks is not in 1..{n_layer}")
        nums = "|".join(str(x) for x in range(peer_blocks))
        print(f"blk\\.({nums})\\.=RPC0[{peer}]")
        return 0
    print(f"unknown: {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
