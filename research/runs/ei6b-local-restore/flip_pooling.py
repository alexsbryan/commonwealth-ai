#!/usr/bin/env python3
"""Repack a snapshot archive with its manifest's declared pooling flipped.

The NEGATIVE control for `judge_restored_snapshot`. Nothing else about the
archive changes — same chunks, same vectors, same ids — so a refusal can only
have come from the declared config. Without this the ConfigMismatch arm is a
gate with no input that can make it fire (ARCH §18.1).
"""
import io, json, sys, tarfile
import zstandard as zstd

src, dst, new_pooling = sys.argv[1], sys.argv[2], (sys.argv[3] if len(sys.argv) > 3 else "mean")
MANIFEST = "_snapshot_manifest.json"

with open(src, "rb") as f:
    raw = zstd.ZstdDecompressor().stream_reader(f).read()

members, flipped = [], False
with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as tf:
    for m in tf.getmembers():
        data = tf.extractfile(m).read() if m.isfile() else None
        if m.name.endswith(MANIFEST):
            man = json.loads(data)
            q = man.get("embed_quirks")
            if not q:
                sys.exit(f"flip: {src} declares no embed_quirks — nothing to flip, and the "
                         f"negative control would prove nothing")
            was = q.get("pooling")
            q["pooling"] = new_pooling
            data = json.dumps(man, indent=2).encode()
            m.size = len(data)
            flipped = True
            print(f"flip: pooling {was!r} -> {new_pooling!r} in {m.name}")
        members.append((m, data))

if not flipped:
    sys.exit(f"flip: no {MANIFEST} in {src}")

buf = io.BytesIO()
with tarfile.open(fileobj=buf, mode="w:") as out:
    for m, data in members:
        out.addfile(m, io.BytesIO(data) if data is not None else None)
with open(dst, "wb") as f:
    f.write(zstd.ZstdCompressor(level=3).compress(buf.getvalue()))
print(f"flip: wrote {dst}")
