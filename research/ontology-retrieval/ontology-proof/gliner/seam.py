"""The v1 GLiNER seam, ported line for line so it can be measured from Python on CPU.

The seam itself cannot be driven from here without a cargo build (G-T0 forbids one): the
`/v1/ner` wire carries no labels (sovereign-contracts/src/ner.rs:190-203, NerRequest is
`texts` + `pass`), the running daemon mounts no `/v1/ner` (GET 404, where its POST routes
/v1/embeddings and /v1/chat/completions answer GET with 405; the process started 2026-10-01
04:45, before the windowing fix f4a695e9a of 2026-10-02 15:17),
and no prebuilt binary links sovereign-gliner with a label argument. So every number from
this module is labelled by what it ran:

  reference  the `gliner` Python package, PyTorch weights (urchade/gliner_small-v2.1)
  seam-onnx  the package's pre/post-processing, with the network's logits taken from the
             ONNX graph the seam loads (~/.svrnmesh/models/gliner/gliner_small-v2.1/onnx/
             model.onnx) on the identical inputs (`OnnxLogits`)
  +seam-post the seam's own post-processing, ported below: 300/30 word windows,
             threshold 0.6, per-text dedupe by (lowercased text, label)

What is NOT ported, so NOT measured: gline-rs's own pre/post-processing (its splitter caps a
window at 512 regex tokens, gline-rs src/text/splitter.rs:42-45; the Python package caps at
the config's max_len 384), and its byte (not char) offsets.

Each constant cites the Rust line it copies.
"""
import pathlib
import re

MODEL_ID = "urchade/gliner_small-v2.1"
SEAM_ONNX = pathlib.Path.home() / ".svrnmesh/models/gliner/gliner_small-v2.1/onnx/model.onnx"

# sovereign-gliner/src/gliner_ner.rs
DEFAULT_THRESHOLD = 0.6                       # :53
DEFAULT_LABELS = ["Person", "Organization", "Work", "Location", "Event"]  # :59
WINDOW_WORDS = 300                            # :453
WINDOW_OVERLAP_WORDS = 30                     # :456
# gline-rs-1.0.1 src/model/params.rs:28-35 (Parameters::default) and src/text/splitter.rs:30
GLINE_THRESHOLD = 0.5
GLINE_MAX_LENGTH = 512
SPLIT_RX = re.compile(r"\w+(?:[-_]\w+)*|\S")


def word_windows(text, max_words=WINDOW_WORDS, overlap=WINDOW_OVERLAP_WORDS):
    """gliner_ner.rs:461 `word_windows`, in CHARACTER offsets (Rust uses bytes)."""
    starts, prev_ws = [], True
    for i, c in enumerate(text):
        if not c.isspace() and prev_ws:
            starts.append(i)
        prev_ws = c.isspace()
    if len(starts) <= max_words:
        return [(0, text)]
    step = max_words - min(overlap, max_words - 1)
    out, first = [], 0
    while True:
        last = first + max_words
        begin = starts[first]
        end = starts[last] if last < len(starts) else len(text)
        out.append((begin, text[begin:end].rstrip()))
        if last >= len(starts):
            break
        first += step
    return out


def normalize(s):
    """gliner_ner.rs:494 `normalize_mention_text`."""
    return " ".join(s.split())


def dedupe_strongest(ms):
    """labeled.rs:59 `dedupe_strongest`: one mention per (lowercased text, label), highest score."""
    out, seen = [], {}
    for m in ms:
        if not m["text"]:
            continue
        key = (m["text"].lower(), m["label"])
        if key in seen:
            if out[seen[key]]["score"] < m["score"]:
                out[seen[key]] = m
            continue
        seen[key] = len(out)
        out.append(m)
    return out


ONNX_INPUTS = ["input_ids", "attention_mask", "words_mask", "text_lengths", "span_idx", "span_mask"]


def _module():
    import torch
    return torch.nn.Module


class OnnxLogits(_module()):
    """The reference model's forward, with its logits REPLACED by the seam's ONNX graph run on
    the identical inputs. gliner 0.2.26's own ONNX path (load_onnx_model=True) returns zero
    spans for this graph: its ORT wrapper's output carries no span_idx/span_mask, which the
    span decoder needs (onnx/model.py:155-157 vs model.py:1370-1376). Running both and keeping
    the ONNX logits also measures the graph against the weights on every batch (`max_diff`)."""

    def __init__(self, torch_model):
        import onnxruntime as ort
        super().__init__()
        self.torch_model = torch_model
        self.sess = ort.InferenceSession(str(SEAM_ONNX), providers=["CPUExecutionProvider"])
        self.max_diff = 0.0
        self.batches = 0

    def forward(self, **kw):
        import numpy as np
        import torch
        out = self.torch_model(**kw)
        feed = {k: kw[k].cpu().numpy() for k in ONNX_INPUTS}
        logits = self.sess.run(["logits"], feed)[0]
        self.max_diff = max(self.max_diff, float(np.abs(logits - out.logits.detach().cpu().numpy()).max()))
        self.batches += 1
        out["logits"] = torch.from_numpy(logits)
        return out

    def __getattr__(self, name):
        try:
            return super().__getattr__(name)
        except AttributeError:
            return getattr(super().__getattr__("torch_model"), name)


def load(arm, max_length=None):
    """arm 'reference' (PyTorch) or 'seam-onnx' (the seam's graph for the network), CPU only."""
    from gliner import GLiNER
    m = GLiNER.from_pretrained(MODEL_ID, map_location="cpu", max_length=max_length)
    if arm == "seam-onnx":
        m.model = OnnxLogits(m.model)
    return m


def infer(model, texts, labels, threshold, windowed=False, dedupe=False, batch_size=8):
    """Mentions per text: {start, end, text, label, score}, char offsets into that text."""
    flat, owner = [], []
    for i, t in enumerate(texts):
        for shift, w in (word_windows(t) if windowed else [(0, t)]):
            flat.append(w)
            owner.append((i, shift))
    raw = model.inference(flat, labels, flat_ner=True, threshold=threshold, batch_size=batch_size)
    out = [[] for _ in texts]
    for (i, shift), ents in zip(owner, raw):
        for e in ents:
            out[i].append({"start": e["start"] + shift, "end": e["end"] + shift,
                           "text": normalize(e["text"]) if dedupe else e["text"],
                           "label": e["label"], "score": float(e["score"])})
    return [dedupe_strongest(ms) if dedupe else ms for ms in out]
