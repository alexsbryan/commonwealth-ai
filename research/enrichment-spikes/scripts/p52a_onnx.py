#!/usr/bin/env python3
"""P5.2a ONNX-export feasibility check (gate G9, second deliverable).

Attempts torch.onnx.export of ColModernVBERT's image-scoring forward with a
real processor-produced input set. Records the outcome either way — the
answer G9 wants is "does the pinned-ort path stay open for this model, and
if not, where exactly does it break".

Usage: .venv/bin/python scripts/p52a_onnx.py --fixture data/p52a --out runs/p52a/onnx
"""

import argparse
import json
import time
import traceback
from pathlib import Path

import torch
from colpali_engine.models import ColModernVBert, ColModernVBertProcessor
from PIL import Image

MODEL_ID = "ModernVBERT/colmodernvbert"


class ImageTower(torch.nn.Module):
    """Fixed-signature wrapper so export sees positional tensor args."""

    def __init__(self, model, keys):
        super().__init__()
        self.model = model
        self.keys = keys

    def forward(self, *tensors):
        return self.model(**dict(zip(self.keys, tensors)))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--fixture", default="data/p52a")
    ap.add_argument("--out", default="runs/p52a/onnx")
    args = ap.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    processor = ColModernVBertProcessor.from_pretrained(MODEL_ID)
    model = ColModernVBert.from_pretrained(
        MODEL_ID, torch_dtype=torch.float32, trust_remote_code=True
    ).eval()

    img = Image.open(Path(args.fixture) / "pages" / "page_00.png")
    inputs = processor.process_images([img])
    keys = list(inputs.keys())
    shapes = {k: list(v.shape) for k, v in inputs.items()}
    print(f"processor image-input keys: {shapes}")

    wrapper = ImageTower(model, keys)
    tensors = tuple(inputs[k] for k in keys)
    with torch.no_grad():
        ref = wrapper(*tensors)
    print(f"eager output: {list(ref.shape)}")

    verdict = {"input_shapes": shapes, "output_shape": list(ref.shape)}
    onnx_path = out / "colmodernvbert_image.onnx"
    t0 = time.time()
    try:
        torch.onnx.export(
            wrapper,
            tensors,
            str(onnx_path),
            input_names=keys,
            output_names=["embeddings"],
            opset_version=17,
            dynamo=False,
        )
        verdict["export"] = "ok"
        verdict["export_s"] = round(time.time() - t0, 1)
        verdict["onnx_mb"] = round(onnx_path.stat().st_size / 1e6, 1)
        print(f"EXPORT OK in {verdict['export_s']}s, {verdict['onnx_mb']} MB")
    except Exception as e:
        verdict["export"] = "failed"
        verdict["error"] = f"{type(e).__name__}: {e}"
        (out / "export_traceback.txt").write_text(traceback.format_exc())
        print(f"TORCHSCRIPT EXPORT FAILED: {verdict['error'][:300]}")
        # Second arm: the dynamo exporter covers ops TorchScript can't.
        t0 = time.time()
        try:
            torch.onnx.export(
                wrapper,
                tensors,
                str(onnx_path),
                input_names=keys,
                output_names=["embeddings"],
                dynamo=True,
            )
            verdict["export_dynamo"] = "ok"
            verdict["export_dynamo_s"] = round(time.time() - t0, 1)
            verdict["onnx_mb"] = round(onnx_path.stat().st_size / 1e6, 1)
            verdict["export"] = "ok"
            print(f"DYNAMO EXPORT OK in {verdict['export_dynamo_s']}s, {verdict['onnx_mb']} MB")
        except Exception as e2:
            verdict["export_dynamo"] = "failed"
            verdict["error_dynamo"] = f"{type(e2).__name__}: {e2}"
            (out / "export_dynamo_traceback.txt").write_text(traceback.format_exc())
            print(f"DYNAMO EXPORT FAILED: {verdict['error_dynamo'][:300]}")

    # Numeric parity check if we got a file (onnxruntime python, informational —
    # the rc.9 RUST load check is a separate 10-line example if this passes).
    if verdict.get("export") == "ok":
        try:
            import onnxruntime as ort_py

            sess = ort_py.InferenceSession(str(onnx_path))
            got = sess.run(None, {k: inputs[k].numpy() for k in keys})[0]
            diff = float(abs(torch.from_numpy(got) - ref).max())
            verdict["max_abs_diff_vs_eager"] = diff
            print(f"onnxruntime parity: max abs diff {diff:.2e}")
        except ModuleNotFoundError:
            verdict["parity"] = "onnxruntime not installed; skipped"

    (out / "verdict.json").write_text(json.dumps(verdict, indent=2))
    print(f"wrote {out / 'verdict.json'}")


if __name__ == "__main__":
    main()
