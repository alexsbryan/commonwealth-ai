#!/usr/bin/env python3
"""Orientation-bench spike — protocol in README.md, questions in bank.toml.

Phases (each checkpointed under out/, resumable):
  nodes  — generate file -> dir -> crate rollup nodes from the leaf cache
  embed  — embed leaves + nodes + questions (content-hash cached)
  score  — arms A-D, mixed + additive policies -> results.json + audit.md
  all    — nodes, embed, score

Usage: python3 spike.py <phase> [--limit N]   (--limit: smoke-test on N files)
"""

import argparse
import hashlib
import json
import os
import re
import sys
import time
from collections import defaultdict
from pathlib import Path

import numpy as np
import requests
import tomllib

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
OUT = HERE / "out"
CACHE = Path.home() / ".sovereign/indexes/commonwealth-ai/code_intel_cache.json"
API = "http://localhost:9741/v1"
SCOPE = "corpus-engine/"
CRATE = "corpus-engine"

# FastShort slot refuses prompt+system > 6000 chars (pick_slot gate); stay under
# with margin. Bigger prompts route to primary instead of truncating.
FAST_CHAR_BUDGET = 5200
PRIMARY_CHAR_BUDGET = 24000
MAX_OUT_TOKENS = 300
TEMPERATURE = 0.2

SYSTEM_FILE = """You write orientation summaries of code for a developer new to the codebase.
You are given a source file's path, its doc header (may be empty), and one-line summaries of its functions.
Reply in exactly this format:
summary: <2-3 plain-English sentences: what this file is for and what a developer finds here. No function-by-function listing. No code jargon where a plain word exists.>
asks: <a question a developer would ask that this file answers>
<a second such question>
Only describe what the inputs evidence. Do not invent capabilities."""

SYSTEM_DIR = """You write orientation summaries of code for a developer new to the codebase.
You are given a module directory's path and one-line summaries of the files and submodules inside it.
Reply in exactly this format:
summary: <2-3 plain-English sentences: what this module is for, how it is organized, and what a developer finds here.>
asks: <a question a developer would ask that this module answers>
<a second such question>
Only describe what the inputs evidence. Do not invent capabilities."""

THINK_RE = re.compile(r"<think>.*?</think>", re.DOTALL)


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def chat(system: str, user: str, model: str) -> str:
    last_err = None
    for attempt in range(3):
        try:
            r = requests.post(
                f"{API}/chat/completions",
                json={
                    "model": model,
                    "max_tokens": MAX_OUT_TOKENS,
                    "temperature": TEMPERATURE,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user},
                    ],
                },
                timeout=600,
            )
            r.raise_for_status()
            text = THINK_RE.sub("", r.json()["choices"][0]["message"]["content"]).strip()
            if text:
                return text
            last_err = "empty completion"
        except Exception as e:  # noqa: BLE001 — retried, then aborts loudly below
            last_err = str(e)
        time.sleep(5 * (attempt + 1))
    raise RuntimeError(f"chat failed after retries ({model}): {last_err}")


def parse_node(text: str) -> tuple[str, list[str], bool]:
    """Return (summary, asks, clean_parse)."""
    m = re.search(r"summary:\s*(.*?)(?:\n\s*asks:\s*\n?(.*))?$", text, re.DOTALL | re.IGNORECASE)
    if not m:
        return text.strip(), [], False
    summary = m.group(1).strip()
    asks = [ln.strip() for ln in (m.group(2) or "").splitlines() if ln.strip()]
    return summary, asks[:2], True


# ---------------------------------------------------------------- tree shape


def load_leaves() -> list[dict]:
    cache = json.loads(CACHE.read_text())
    leaves = []
    for e in cache:
        fp = e["meta"]["file_path"]
        if not fp.startswith(SCOPE):
            continue
        text = e["summary"]
        if e.get("asks"):
            text += "\n" + "\n".join(e["asks"])
        leaves.append(
            {
                "id": f"leaf:{e['meta']['qualified_name'] or fp + '#' + e['meta']['name']}",
                "tier": "leaf",
                "path": fp,
                "name": e["meta"]["name"],
                "line": e["meta"]["line_start"],
                "text": text,
                "summary_only": e["summary"],
            }
        )
    return leaves


def node_dirs(files: list[str]) -> list[str]:
    """Every ancestor dir of the given files below the crate root, except
    <crate>/src (merged into the crate node). Deepest first."""
    dirs = set()
    for f in files:
        p = Path(f).parent
        while str(p) not in (CRATE, f"{CRATE}/src", "."):
            dirs.add(str(p))
            p = p.parent
    return sorted(dirs, key=lambda d: d.count("/"), reverse=True)


def doc_header(file_path: str, max_lines: int = 15) -> str:
    lines = []
    try:
        with open(REPO / file_path, encoding="utf-8", errors="replace") as fh:
            for ln in fh:
                s = ln.strip()
                if s.startswith("//!"):
                    lines.append(s[3:].strip())
                    if len(lines) >= max_lines:
                        break
                elif lines or (s and not s.startswith("//") and not s.startswith("#!")):
                    if lines:
                        break
    except OSError:
        pass
    return "\n".join(lines)


# ------------------------------------------------------------- nodes phase


def build_prompt(path: str, header: str, child_lines: list[str], is_dir: bool) -> tuple[str, str, str]:
    """Return (system, user, model). Model escalates fast->primary on size —
    no silent truncation; if even primary's budget overflows, tail-drop and SAY SO."""
    body = f"path: {path}\n"
    if header:
        body += f"doc header:\n{header}\n"
    body += "contents:\n" + "\n".join(child_lines)
    system = SYSTEM_DIR if is_dir else SYSTEM_FILE
    total = len(system) + len(body)
    if total <= FAST_CHAR_BUDGET:
        return system, body, "fast"
    if total <= PRIMARY_CHAR_BUDGET:
        return system, body, "primary"
    keep = []
    budget = PRIMARY_CHAR_BUDGET - len(system) - 200
    used = len(body) - sum(len(c) + 1 for c in child_lines)
    for c in child_lines:
        if used + len(c) + 1 > budget:
            break
        keep.append(c)
        used += len(c) + 1
    dropped = len(child_lines) - len(keep)
    log(f"  OVERFLOW {path}: dropped {dropped}/{len(child_lines)} child lines (primary budget)")
    keep.append(f"[... {dropped} further entries omitted for length ...]")
    body = f"path: {path}\n" + (f"doc header:\n{header}\n" if header else "") + "contents:\n" + "\n".join(keep)
    return system, body, "primary"


def phase_nodes(limit: int | None) -> None:
    OUT.mkdir(exist_ok=True)
    nodes_path = OUT / "nodes.json"
    done: dict[str, dict] = json.loads(nodes_path.read_text()) if nodes_path.exists() else {}
    teasers = open(OUT / "drift_teasers.jsonl", "a", encoding="utf-8")

    leaves = load_leaves()
    by_file = defaultdict(list)
    for lf in leaves:
        by_file[lf["path"]].append(lf)
    files = sorted(by_file)
    if limit:
        files = files[:limit]
    log(f"leaves={len(leaves)} files={len(files)} (limit={limit})")

    def save() -> None:
        nodes_path.write_text(json.dumps(done, indent=1))

    stats = {"fast": 0, "primary": 0, "parse_warn": 0}

    # file nodes
    for i, fp in enumerate(files):
        key = f"node:{fp}"
        if key in done:
            continue
        kids = sorted(by_file[fp], key=lambda x: x["line"])
        child_lines = [f"fn {k['name']}: {k['summary_only']}" for k in kids]
        header = doc_header(fp)
        system, user, model = build_prompt(fp, header, child_lines, is_dir=False)
        text = chat(system, user, model)
        summary, asks, clean = parse_node(text)
        if not clean:
            stats["parse_warn"] += 1
        stats[model] += 1
        done[key] = {
            "id": key, "tier": "file", "path": fp, "model": model,
            "children": len(child_lines), "summary": summary, "asks": asks,
            "text": summary + ("\n" + "\n".join(asks) if asks else ""),
            "clean_parse": clean,
        }
        if header:
            teasers.write(json.dumps({"path": fp, "asserted": header, "derived": summary}) + "\n")
            teasers.flush()
        if (i + 1) % 10 == 0 or i == len(files) - 1:
            save()
            log(f"file nodes {i + 1}/{len(files)} (fast={stats['fast']} primary={stats['primary']} warn={stats['parse_warn']})")
    save()

    if limit:
        log("limit set — skipping dir/crate nodes in smoke mode")
        return

    # dir nodes, deepest first, from child file/dir node summaries
    dirs = node_dirs(files)
    log(f"dir nodes to build: {len(dirs)}")
    for d in dirs:
        key = f"node:{d}"
        if key in done:
            continue
        child_lines = []
        for k, n in sorted(done.items()):
            parent = str(Path(n["path"]).parent)
            if n["tier"] == "file" and parent == d:
                child_lines.append(f"file {Path(n['path']).name}: {n['summary']}")
            elif n["tier"] == "dir" and parent == d:
                child_lines.append(f"module {Path(n['path']).name}/: {n['summary']}")
        if not child_lines:
            raise RuntimeError(f"dir node {d} has no children — tree bug, aborting loudly")
        header = doc_header(f"{d}/mod.rs")
        system, user, model = build_prompt(d, header, child_lines, is_dir=True)
        text = chat(system, user, model)
        summary, asks, clean = parse_node(text)
        done[key] = {
            "id": key, "tier": "dir", "path": d, "model": model,
            "children": len(child_lines), "summary": summary, "asks": asks,
            "text": summary + ("\n" + "\n".join(asks) if asks else ""),
            "clean_parse": clean,
        }
        if header:
            teasers.write(json.dumps({"path": d, "asserted": header, "derived": summary}) + "\n")
            teasers.flush()
        save()
        log(f"dir node {d} ({len(child_lines)} children, {model})")

    # crate node from everything directly under <crate>/src
    key = f"node:{CRATE}"
    if key not in done:
        child_lines = []
        for k, n in sorted(done.items()):
            parent = str(Path(n["path"]).parent)
            if parent in (f"{CRATE}/src", CRATE):
                tag = "module" if n["tier"] == "dir" else "file"
                child_lines.append(f"{tag} {Path(n['path']).name}: {n['summary']}")
        header = doc_header(f"{CRATE}/src/lib.rs")
        system, user, model = build_prompt(CRATE, header, child_lines, is_dir=True)
        text = chat(system, user, model)
        summary, asks, clean = parse_node(text)
        done[key] = {
            "id": key, "tier": "crate", "path": CRATE, "model": model,
            "children": len(child_lines), "summary": summary, "asks": asks,
            "text": summary + ("\n" + "\n".join(asks) if asks else ""),
            "clean_parse": clean,
        }
        save()
        log(f"crate node built ({len(child_lines)} children, {model})")
    tiers = defaultdict(int)
    for n in done.values():
        tiers[n["tier"]] += 1
    log(f"nodes done: {dict(tiers)}")


# ------------------------------------------------------------- embed phase


def embed_texts(texts: list[str], batch: int = 32) -> np.ndarray:
    vecs = []
    for i in range(0, len(texts), batch):
        chunk = texts[i : i + batch]
        for attempt in range(3):
            try:
                r = requests.post(
                    f"{API}/embeddings",
                    json={"model": "embed", "input": chunk},
                    timeout=300,
                )
                r.raise_for_status()
                data = sorted(r.json()["data"], key=lambda d: d["index"])
                vecs.extend(d["embedding"] for d in data)
                break
            except Exception as e:  # noqa: BLE001
                if attempt == 2:
                    raise
                log(f"embed batch retry ({e})")
                time.sleep(10)
        if (i // batch) % 20 == 0:
            log(f"embedded {min(i + batch, len(texts))}/{len(texts)}")
    return np.asarray(vecs, dtype=np.float32)


def pool_items() -> tuple[list[dict], list[dict]]:
    leaves = load_leaves()
    nodes = list(json.loads((OUT / "nodes.json").read_text()).values())
    return leaves, nodes


def phase_embed() -> None:
    leaves, nodes = pool_items()
    bank = tomllib.loads((HERE / "bank.toml").read_text())
    questions = bank["question"]
    items = (
        [(lf["id"], lf["text"]) for lf in leaves]
        + [(n["id"], n["text"]) for n in nodes]
        + [(f"q:{q['id']}", q["text"]) for q in questions]
    )
    ids = [i for i, _ in items]
    hashes = {i: hashlib.sha256(t.encode()).hexdigest() for i, t in items}

    old_ids, old_hashes, old_mat = [], {}, None
    npz = OUT / "embeds.npz"
    meta = OUT / "embeds_meta.json"
    if npz.exists() and meta.exists():
        m = json.loads(meta.read_text())
        old_ids, old_hashes = m["ids"], m["hashes"]
        old_mat = np.load(npz)["mat"]
    reuse = {i: old_ids.index(i) for i in ids if i in old_hashes and old_hashes[i] == hashes[i]} if old_ids else {}
    todo = [(i, t) for i, t in items if i not in reuse]
    log(f"embed: {len(reuse)} cached, {len(todo)} to embed")
    new_mat = embed_texts([t for _, t in todo]) if todo else np.zeros((0, 1024), np.float32)
    dim = new_mat.shape[1] if len(new_mat) else old_mat.shape[1]
    mat = np.zeros((len(ids), dim), np.float32)
    ti = 0
    for row, i in enumerate(ids):
        if i in reuse:
            mat[row] = old_mat[reuse[i]]
        else:
            mat[row] = new_mat[ti]
            ti += 1
    mat /= np.linalg.norm(mat, axis=1, keepdims=True).clip(min=1e-9)
    np.savez_compressed(npz, mat=mat)
    meta.write_text(json.dumps({"ids": ids, "hashes": hashes}))
    log(f"embeds saved: {mat.shape}")


# ------------------------------------------------------------- score phase


def matches(item_path: str, gold: str) -> bool:
    return item_path == gold or item_path.startswith(gold.rstrip("/") + "/")


def hit_rank(ranked: list[dict], golds: list[str], k: int) -> int | None:
    for r, it in enumerate(ranked[:k], 1):
        if any(matches(it["path"], g) for g in golds):
            return r
    return None


def phase_score() -> None:
    leaves, nodes = pool_items()
    bank = tomllib.loads((HERE / "bank.toml").read_text())
    if not bank["meta"].get("frozen"):
        raise RuntimeError("bank.toml is not frozen — freeze before scoring (protocol)")
    sc = bank["meta"]["scoring"]
    K, SMIN, NEGK = sc["hit_k"], sc["structure_min_distinct"], sc["negative_node_flag_k"]
    questions = bank["question"]

    meta = json.loads((OUT / "embeds_meta.json").read_text())
    mat = np.load(OUT / "embeds.npz")["mat"]
    row = {i: r for r, i in enumerate(meta["ids"])}
    all_items = leaves + nodes
    arms = {
        "A": [it for it in all_items if it["tier"] == "leaf"],
        "B": [it for it in all_items if it["tier"] in ("leaf", "file")],
        "C": all_items,
        "D": [it for it in all_items if it["tier"] != "leaf"],
    }
    arm_rows = {a: np.array([row[it["id"]] for it in items]) for a, items in arms.items()}

    results = defaultdict(lambda: defaultdict(list))
    audit = ["# Orientation-bench audit — per-question top ranks\n"]
    neg_flags = []
    guard_ranks = defaultdict(dict)

    for q in questions:
        qv = mat[row[f"q:{q['id']}"]]
        audit.append(f"\n## {q['id']} ({q['shape']}) — {q['text']}\ngold: {q['gold'] or q.get('answer_hint', '')}\n")
        per_arm_ranked = {}
        for a, items in arms.items():
            sims = mat[arm_rows[a]] @ qv
            order = np.argsort(-sims)[:10]
            ranked = [dict(items[j], score=float(sims[j])) for j in order]
            per_arm_ranked[a] = ranked

            if q["shape"] == "structure":
                covered = {g for g in q["gold"] for it in ranked[:K] if matches(it["path"], g)}
                hit, rank = len(covered) >= SMIN, None
            elif q["shape"] == "negative":
                hit, rank = None, None
            else:
                rank = hit_rank(ranked, q["gold"], 10)
                hit = rank is not None and rank <= K
            results[a][q["shape"]].append({"id": q["id"], "hit": hit, "rank": rank})
            if q["shape"] == "guardrail":
                guard_ranks[q["id"]][a] = rank
            if q["shape"] == "negative" and a == "C":
                flags = [it for it in ranked[:NEGK] if it["tier"] != "leaf"]
                if flags:
                    neg_flags.append({"id": q["id"], "nodes_in_top3": [f["path"] for f in flags]})

        # additive policy on C: top-K leaves + top-2 nodes
        add = per_arm_ranked["A"][:K] + [it for it in per_arm_ranked["D"] if it["tier"] != "leaf"][:2]
        if q["shape"] not in ("structure", "negative"):
            r_add = hit_rank(add, q["gold"], len(add))
            results["C_additive"][q["shape"]].append({"id": q["id"], "hit": r_add is not None, "rank": r_add})

        for a in ("A", "C"):
            audit.append(f"\n**arm {a}** top-5:\n")
            for r, it in enumerate(per_arm_ranked[a][:5], 1):
                mark = "HIT" if q["gold"] and any(matches(it["path"], g) for g in q["gold"]) else "   "
                label = it["path"] if it["tier"] != "leaf" else f"{it['path']} :: {it.get('name', '')}"
                audit.append(f"{r}. [{mark}] ({it['tier']}) {label}  {it['score']:.3f}")

    summary = {"arms": {}, "guardrail_rank_shift": {}, "negative_flags": neg_flags}
    for a, shapes in results.items():
        summary["arms"][a] = {}
        for shape, rows in shapes.items():
            hits = [r for r in rows if r["hit"] is not None]
            mrr = np.mean([1 / r["rank"] if r["rank"] else 0 for r in rows])
            summary["arms"][a][shape] = {
                "n": len(rows),
                "hit_at_5": round(sum(bool(r["hit"]) for r in hits) / max(len(hits), 1), 3),
                "mrr": round(float(mrr), 3),
            }
    for qid, ranks in guard_ranks.items():
        summary["guardrail_rank_shift"][qid] = {a: ranks.get(a) for a in ("A", "B", "C")}

    tiers = defaultdict(int)
    for n in nodes:
        tiers[n["tier"]] += 1
    summary["pool"] = {"leaves": len(leaves), **tiers}
    (OUT / "results.json").write_text(json.dumps(summary, indent=1))
    (OUT / "audit.md").write_text("\n".join(audit))
    log(json.dumps(summary["arms"], indent=1))
    log(f"results.json + audit.md written; negative flags: {len(neg_flags)}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("phase", choices=["nodes", "embed", "score", "all"])
    ap.add_argument("--limit", type=int, default=None)
    args = ap.parse_args()
    if args.phase in ("nodes", "all"):
        phase_nodes(args.limit)
    if args.phase in ("embed", "all"):
        phase_embed()
    if args.phase in ("score", "all"):
        phase_score()
