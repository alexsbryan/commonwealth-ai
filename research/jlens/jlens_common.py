"""Shared machinery for the J-lens workspace replication on Qwen3-8B.

Core objects:
  load_model()            -- tokenizer + frozen bf16 model on MPS
  forward_with_capture()  -- one forward that exposes every layer's residual
                             to autograd despite frozen params
  derive_jlens()          -- J-lens vectors: grad of concept log-prob w.r.t.
                             each layer residual, averaged over contexts
  JLensPack               -- derived artifact (vectors + calibration stats)
  readout_z()             -- z-scored concept readout at one layer
  Injector                -- forward hooks adding scaled vectors to residuals
                             (the PyTorch twin of llama.cpp set_adapter_cvec)
  chat_generate()         -- greedy chat-template generation, thinking off
"""

import os

os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

import json
import hashlib
from dataclasses import dataclass, field

import torch

MODEL_ID = "Qwen/Qwen3-8B"
DEVICE = "mps" if torch.backends.mps.is_available() else "cpu"
DTYPE = torch.bfloat16
OUT_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
PACK_PATH = os.path.join(OUT_DIR, "jlens_qwen3-8b.pt")

_cache = {}


def load_model():
    if "model" not in _cache:
        from transformers import AutoModelForCausalLM, AutoTokenizer

        tok = AutoTokenizer.from_pretrained(MODEL_ID)
        if tok.pad_token is None:
            tok.pad_token = tok.eos_token
        tok.padding_side = "left"  # keeps every row's final position at -1
        model = AutoModelForCausalLM.from_pretrained(MODEL_ID, dtype=DTYPE)
        model.to(DEVICE).eval()
        model.requires_grad_(False)
        _cache["tok"], _cache["model"] = tok, model
    return _cache["tok"], _cache["model"]


def decoder_layers(model):
    return model.model.layers


# ---------------------------------------------------------------- concepts

CONCEPT_WORDS = [
    # animals
    "elephant", "giraffe", "spider", "dog", "cat", "lion", "whale", "eagle",
    "snake", "horse", "tiger", "monkey", "rabbit", "shark", "wolf", "penguin",
    # fruits
    "apple", "orange", "lemon", "banana", "grape", "mango", "peach", "cherry",
    # places
    "France", "Japan", "Brazil", "Egypt", "Canada", "India", "Russia",
    "Texas", "London", "Paris", "Africa", "Antarctica",
    # colors
    "red", "blue", "green", "yellow", "purple", "white", "black",
    # objects
    "piano", "hammer", "bicycle", "mirror", "candle", "guitar", "clock",
    "knife", "ladder", "umbrella",
    # abstract
    "justice", "freedom", "money", "music", "war", "love", "fear", "truth",
    # exp-c answer words (need J-lens vectors for swap targets/answers)
    "French", "Japanese", "yen", "euro",
]


def single_token_concepts(tok):
    """Concepts whose ' word' form is one token — the J-lens unit. Returns
    (words, token_ids) in stable order."""
    words, ids = [], []
    for w in CONCEPT_WORDS:
        enc = tok.encode(" " + w, add_special_tokens=False)
        if len(enc) == 1:
            words.append(w)
            ids.append(enc[0])
    return words, ids


# ---------------------------------------------------------------- contexts

# Diverse neutral contexts for Jacobian averaging + readout calibration.
# Deliberately spread across register and topic; none mention the concepts
# in a way that should dominate the average.
CONTEXTS = [
    "The committee voted to postpone the decision until the next quarterly meeting.",
    "Whisk the eggs with a pinch of salt before folding in the flour.",
    "Interest rates rose again this spring, cooling the housing market slightly.",
    "The hikers reached the ridge just as the fog began to lift.",
    "Her latest novel opens with a funeral and ends with a wedding.",
    "The firmware update fixed the bootloader but broke the sleep timer.",
    "Rainfall totals this month were the highest recorded in a decade.",
    "He apologized for the delay and offered everyone a refund.",
    "The museum's new wing will house the photography collection.",
    "Traffic on the bridge was backed up for nearly two hours.",
    "The recipe calls for slow roasting at a low temperature overnight.",
    "Negotiations stalled after both sides rejected the draft agreement.",
    "The choir rehearsed in the basement while the roof was repaired.",
    "Quarterly earnings beat expectations despite weaker overseas sales.",
    "She repotted the ferns and moved them away from the radiator.",
    "The referee added four minutes of stoppage time in the second half.",
    "A software bug delayed thousands of flights on Friday morning.",
    "The lecture covered the causes of the industrial revolution.",
    "They repainted the fence a pale shade that matched the shutters.",
    "The clinic extended its hours to accommodate weekend patients.",
    "Volunteers spent the morning clearing brush from the trailhead.",
    "The senator's speech focused on infrastructure and rural broadband.",
    "Fresh snow made the morning commute slower than usual.",
    "The startup pivoted twice before finding a profitable niche.",
    "Grandpa tells the same story at every holiday dinner.",
    "The orchestra's tour includes stops in twelve cities.",
    "New regulations require clearer labeling on packaged foods.",
    "The tide pools were full of small creatures at low tide.",
    "He finally fixed the squeaky hinge with a drop of oil.",
    "The archaeologists catalogued each fragment before moving it.",
    "Enrollment in evening classes doubled after the schedule change.",
    "The bakery sells out of sourdough by nine most mornings.",
    "Cloud cover kept temperatures mild throughout the weekend.",
    "The jury deliberated for three days before reaching a verdict.",
    "She sketched the skyline from the rooftop cafe at dusk.",
    "The warehouse switched to electric forklifts last year.",
    "His presentation ran long, so questions were moved to email.",
    "The river crested two feet below the flood stage.",
    "They argued amiably about the best route through the mountains.",
    "The library's reading room reopened after renovations.",
    "A vendor at the market sells honey from rooftop hives.",
    "The play's second act takes place entirely in a train car.",
    "Engineers rerouted power while the substation was serviced.",
    "The children built an elaborate fort out of couch cushions.",
    "Her thesis examines migration patterns in coastal towns.",
    "The gym replaced its treadmills and added a rowing machine.",
    "Local farms reported a strong harvest despite the dry summer.",
    "The ferry schedule changes twice a year with the seasons.",
]


def contexts_hash():
    return hashlib.sha256("\n".join(CONTEXTS).encode()).hexdigest()[:12]


# ------------------------------------------------------- capture + gradients


class _Capture:
    """Hooks that let autograd reach the residual stream of a frozen model.

    The embedding output is a graph leaf (params frozen -> no grad_fn), so
    flipping requires_grad on it builds the graph for everything above; each
    decoder layer's output tensor is stashed for autograd.grad targets.
    """

    def __init__(self, model):
        self.model = model
        self.handles = []
        self.resids = []

    def __enter__(self):
        def embed_hook(_mod, _inp, out):
            out.requires_grad_(True)
            return out

        self.handles.append(
            self.model.model.embed_tokens.register_forward_hook(embed_hook)
        )

        def layer_hook(_mod, _inp, out):
            h = out[0] if isinstance(out, tuple) else out
            self.resids.append(h)
            return out

        for layer in decoder_layers(self.model):
            self.handles.append(layer.register_forward_hook(layer_hook))
        return self

    def __exit__(self, *exc):
        for h in self.handles:
            h.remove()
        return False


def forward_with_capture(model, input_ids, attention_mask):
    """Returns (last-position logits [B, V], resids: list per layer of [B, T, H])."""
    with _Capture(model) as cap, torch.enable_grad():
        out = model(input_ids=input_ids, attention_mask=attention_mask, use_cache=False)
    return out.logits[:, -1, :], cap.resids


# ---------------------------------------------------------------- pack


@dataclass
class JLensPack:
    model_id: str
    concepts: list  # words
    token_ids: list
    layers: list  # captured layer indices (HF 0-based)
    vectors: dict = field(default_factory=dict)  # layer -> [C, H] unit f32 cpu
    calib_mu: dict = field(default_factory=dict)  # layer -> [C]
    calib_sd: dict = field(default_factory=dict)  # layer -> [C]
    resid_norm: dict = field(default_factory=dict)  # layer -> float (median ||h||)
    ctx_hash: str = ""

    def save(self, path=PACK_PATH):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        torch.save(self.__dict__, path)

    @classmethod
    def load(cls, path=PACK_PATH):
        d = torch.load(path, map_location="cpu", weights_only=False)
        p = cls(model_id=d["model_id"], concepts=d["concepts"],
                token_ids=d["token_ids"], layers=d["layers"])
        p.vectors, p.calib_mu, p.calib_sd = d["vectors"], d["calib_mu"], d["calib_sd"]
        p.resid_norm, p.ctx_hash = d["resid_norm"], d["ctx_hash"]
        return p

    def concept_index(self, word):
        return self.concepts.index(word)

    def vec(self, layer, word):
        return self.vectors[layer][self.concept_index(word)]


def derive_jlens(model, tok, words, token_ids, contexts, batch_size=16,
                 progress=print):
    """J-lens vectors for every layer: mean over contexts of
    d log p(concept token @ next position) / d resid(layer, last position)."""
    n_layers = len(decoder_layers(model))
    hidden = model.config.hidden_size
    acc = {l: torch.zeros(len(words), hidden, dtype=torch.float32)
           for l in range(n_layers)}
    n_batches = 0
    for start in range(0, len(contexts), batch_size):
        batch = contexts[start:start + batch_size]
        enc = tok(batch, return_tensors="pt", padding=True).to(DEVICE)
        logits, resids = forward_with_capture(model, enc.input_ids, enc.attention_mask)
        logprobs = torch.log_softmax(logits.float(), dim=-1)
        for ci, tid in enumerate(token_ids):
            loss = logprobs[:, tid].sum()
            grads = torch.autograd.grad(loss, resids, retain_graph=True)
            for l, g in enumerate(grads):
                acc[l][ci] += g[:, -1, :].float().mean(dim=0).cpu()
        del logits, resids, logprobs
        n_batches += 1
        progress(f"  derived batch {n_batches}/{(len(contexts) + batch_size - 1) // batch_size}")
    vectors = {}
    for l in range(n_layers):
        v = acc[l] / n_batches
        vectors[l] = torch.nn.functional.normalize(v, dim=-1)
    return vectors


def calibrate(model, tok, vectors, contexts, batch_size=16):
    """Per-(layer, concept) readout stats over neutral contexts, plus the
    median residual norm per layer (used to scale injections)."""
    layers = sorted(vectors.keys())
    dots = {l: [] for l in layers}
    norms = {l: [] for l in layers}
    with torch.no_grad():
        for start in range(0, len(contexts), batch_size):
            batch = contexts[start:start + batch_size]
            enc = tok(batch, return_tensors="pt", padding=True).to(DEVICE)
            with _Capture(model) as cap:
                model(input_ids=enc.input_ids, attention_mask=enc.attention_mask,
                      use_cache=False)
            for l in layers:
                h = cap.resids[l][:, -1, :].float().cpu()  # [B, H]
                dots[l].append(h @ vectors[l].T)  # [B, C]
                norms[l].append(h.norm(dim=-1))
    mu, sd, rnorm = {}, {}, {}
    for l in layers:
        d = torch.cat(dots[l])
        mu[l], sd[l] = d.mean(dim=0), d.std(dim=0).clamp_min(1e-6)
        rnorm[l] = torch.cat(norms[l]).median().item()
    return mu, sd, rnorm


def readout_z(pack, layer, h):
    """z-scored concept readout for residual h [H] (float32 cpu) at layer."""
    dots = pack.vectors[layer] @ h
    return (dots - pack.calib_mu[layer]) / pack.calib_sd[layer]


def capture_resids(model, tok, text, layers):
    """No-grad single-prompt forward; returns {layer: [T, H] float32 cpu}."""
    enc = tok(text, return_tensors="pt").to(DEVICE)
    with torch.no_grad(), _Capture(model) as cap:
        model(input_ids=enc.input_ids, attention_mask=enc.attention_mask,
              use_cache=False)
    return {l: cap.resids[l][0].float().cpu() for l in layers}


# ---------------------------------------------------------------- injection


class Injector:
    """Adds a fixed vector to given layers' residual output at every position
    — the same shape of intervention as llama.cpp's control vectors."""

    def __init__(self, model, layer_vecs):
        """layer_vecs: {layer_idx: [H] tensor, already scaled}."""
        self.model = model
        self.layer_vecs = {
            l: v.to(DEVICE, DTYPE) for l, v in layer_vecs.items()
        }
        self.handles = []

    def __enter__(self):
        layers = decoder_layers(self.model)
        for l, vec in self.layer_vecs.items():
            def make(v):
                def hook(_mod, _inp, out):
                    if isinstance(out, tuple):
                        return (out[0] + v, *out[1:])
                    return out + v
                return hook
            self.handles.append(layers[l].register_forward_hook(make(vec)))
        return self

    def __exit__(self, *exc):
        for h in self.handles:
            h.remove()
        return False


def band_inject(pack, word, layers, alpha):
    """{layer: alpha * median_resid_norm(layer) * unit_vec} for an Injector."""
    return {l: pack.vec(l, word) * (alpha * pack.resid_norm[l]) for l in layers}


# ---------------------------------------------------------------- chat


def chat_prompt(tok, user, system=None):
    msgs = ([{"role": "system", "content": system}] if system else [])
    msgs.append({"role": "user", "content": user})
    return tok.apply_chat_template(
        msgs, tokenize=False, add_generation_prompt=True, enable_thinking=False
    )


def chat_generate(model, tok, user, system=None, max_new_tokens=24,
                  layer_vecs=None):
    """Greedy generation; layer_vecs (optional) injects during the whole run."""
    text = chat_prompt(tok, user, system)
    enc = tok(text, return_tensors="pt").to(DEVICE)
    ctx = Injector(model, layer_vecs) if layer_vecs else _null_ctx()
    with torch.no_grad(), ctx:
        out = model.generate(
            **enc, max_new_tokens=max_new_tokens, do_sample=False,
            pad_token_id=tok.pad_token_id,
        )
    return tok.decode(out[0][enc.input_ids.shape[1]:], skip_special_tokens=True).strip()


class _null_ctx:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


def generate_with_resids(model, tok, user, system=None, max_new_tokens=32,
                         layers=(), layer_vecs=None):
    """Greedy generation that also returns per-generated-token residuals at
    the requested layers: (text, {layer: [n_new, H] float32 cpu}).

    Uses stepwise decode without KV cache reuse across the capture (simple,
    slower, fine for small n)."""
    text = chat_prompt(tok, user, system)
    enc = tok(text, return_tensors="pt").to(DEVICE)
    ids = enc.input_ids
    collected = {l: [] for l in layers}
    new_tokens = []
    ctx = Injector(model, layer_vecs) if layer_vecs else _null_ctx()
    with torch.no_grad(), ctx:
        for _ in range(max_new_tokens):
            with _Capture(model) as cap:
                out = model(input_ids=ids, use_cache=False)
            for l in layers:
                collected[l].append(cap.resids[l][0, -1, :].float().cpu())
            nxt = out.logits[0, -1, :].argmax()
            if nxt.item() == tok.eos_token_id:
                break
            new_tokens.append(nxt.item())
            ids = torch.cat([ids, nxt.view(1, 1)], dim=1)
    resids = {l: torch.stack(v) if v else torch.zeros(0) for l, v in collected.items()}
    return tok.decode(new_tokens, skip_special_tokens=True).strip(), resids


# ---------------------------------------------------------------- misc


def save_json(name, obj):
    os.makedirs(OUT_DIR, exist_ok=True)
    path = os.path.join(OUT_DIR, name)
    with open(path, "w") as f:
        json.dump(obj, f, indent=2)
    print(f"wrote {path}")


def mid_band(n_layers):
    """Default mid-layer band: the middle third, biased slightly late —
    where the paper found workspace content (L38-L92 of 100)."""
    return list(range(int(n_layers * 0.40), int(n_layers * 0.75)))
