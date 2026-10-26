#!/usr/bin/env python3
"""Give jina-reranker-v3's GGUF the head and template llama-server reranks with.

    rerank-head-gguf.py <in.gguf> <out.gguf> [--gguf-py DIR]

The published GGUF has no classifier head and no rerank template, so
llama-server's rank pooling scores softmax(last hidden state)[0], about 1e-6
for every document. jina's protocol, which the embedded slot reads
(`RerankProtocol::JinaScoreToken`, rerank_slot.rs), is the <|score_token|>
logit at a trailing <|rerank_token|>. The lm_head is tied to token_embd, so
that logit is token_embd[score] . h. A two-row `cls.output.weight`,
[token_embd[score], 0], makes qwen3's rank softmax answer sigmoid(logit), and
the daemon's ln(p/(1-p)) (oicp_client::rerank) gives the logit back.

The template is the embedded slot's "verbose" scaffold, character for
character (`jina_v3_prompt_fragments`), so both engines score the same
prompt until the embedded one is deleted. Every other key and tensor is
copied unchanged, and the embedded slot ignores the added head.

`--gguf-py` is the pinned llama.cpp's gguf-py (build.sh unpacks it under
target/llama-server/src-*/); by default the first one found there is used.
"""
import argparse
import glob
import os
import sys

PREFIX = ("<|im_start|>system\nYou are a search relevance assessor. Read the query and document, "
          "then judge how relevant the document is to the query.\n<|im_end|>\n<|im_start|>user\n<query>\n")
MIDDLE = "\n</query>\n<document>\n"
SUFFIX = "\n</document>\n<|im_end|>\n<|im_start|>assistant\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("src")
    ap.add_argument("dst")
    ap.add_argument("--gguf-py")
    a = ap.parse_args()
    repo = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../../.."))
    found = sorted(glob.glob(os.path.join(repo, "target/llama-server/src-*/gguf-py")))
    path = a.gguf_py or (found[0] if found else None)
    if not path:
        sys.exit("rerank-head-gguf: no gguf-py; run build.sh first or pass --gguf-py")
    sys.path.insert(0, path)
    import numpy as np
    import gguf

    r = gguf.GGUFReader(a.src)
    arch = r.fields["general.architecture"].contents()
    if f"{arch}.classifier.output_labels" in r.fields:
        sys.exit(f"rerank-head-gguf: {a.src} already has a classifier head")
    tokens = r.fields["tokenizer.ggml.tokens"].contents()
    if "<|score_token|>" not in tokens or "<|rerank_token|>" not in tokens:
        sys.exit(f"rerank-head-gguf: {a.src} does not speak jina's score-token protocol")
    score = tokens.index("<|score_token|>")
    embd = next(t for t in r.tensors if t.name == "token_embd.weight")
    row = gguf.quants.dequantize(embd.data, embd.tensor_type)[score].astype(np.float32)
    head = np.stack([row, np.zeros_like(row)])  # ggml ne = [n_embd, 2]

    w = gguf.GGUFWriter(a.dst, arch)
    for f in r.fields.values():
        if f.name == "general.architecture" or f.name.startswith("GGUF."):
            continue
        t = f.types[0]
        w.add_key_value(f.name, f.contents(), t,
                        sub_type=f.types[-1] if t == gguf.GGUFValueType.ARRAY else None)
    w.add_key_value(f"{arch}.classifier.output_labels", ["score", "par"],
                    gguf.GGUFValueType.ARRAY, sub_type=gguf.GGUFValueType.STRING)
    w.add_key_value("tokenizer.chat_template.rerank",
                    PREFIX + "{query}" + MIDDLE + "{document}" + SUFFIX + "<|rerank_token|>",
                    gguf.GGUFValueType.STRING)
    for t in r.tensors:
        w.add_tensor_info(t.name, t.data.shape, t.data.dtype, t.data.nbytes, t.tensor_type)
    w.add_tensor_info("cls.output.weight", head.shape, head.dtype, head.nbytes,
                      gguf.GGMLQuantizationType.F32)
    w.write_header_to_file()
    w.write_kv_data_to_file()
    w.write_ti_data_to_file()
    for t in r.tensors:
        w.write_tensor_data(t.data, tensor_endianess=r.endianess)
    w.write_tensor_data(head)
    w.close()
    print(f"{a.dst}: score token {score}, cls.output.weight {head.shape}")


if __name__ == "__main__":
    main()
