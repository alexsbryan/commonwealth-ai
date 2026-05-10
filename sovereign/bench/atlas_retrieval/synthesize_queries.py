# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "tqdm>=4.66",
# ]
# ///
"""Synthesize a labeled query set from an atlas-enriched corpus.

Reads atoms.json / edges.json / trajectories.json / tension_candidates.json
from ~/.sovereign/indexes/<corpus>/atlas/ and emits one query per atom-class
we care about, with ground-truth chunk ids drawn from each atom's provenance.

Query classes map 1:1 to atom/edge kinds so the bench can stratify recall:
    entity         — "who/what is X" derived from Entity atoms
    event          — "what happens in X" derived from Event atoms
    relation       — "how are X and Y related" derived from Relation atoms
    claim          — "what is argued about X" derived from Claim atoms
    question       — verbatim from Question atoms
    tension        — "how do these views conflict on X" from tension_candidates
    trajectory     — "how does X evolve" from Trajectory atoms

Ground truth is emitted as a list of *section ids* (sec_NNNN) — the bench
evaluates recall by checking whether a returned paragraph chunk's section_id
is in the ground-truth set.

--paraphrase <model> enables LLM-driven paraphrasing per query via the
daemon's /v1/chat/completions endpoint; default is the deterministic template
path for fast iteration.
"""
from __future__ import annotations

import argparse
import json
import random
import re
import sys
import time
from pathlib import Path

import httpx
from tqdm import tqdm


SOVEREIGN_ROOT = Path.home() / ".sovereign"
DAEMON_URL = "http://127.0.0.1:9741"

# ─── Atom extraction ──────────────────────────────────────────────────


def atlas_path(corpus: str, name: str) -> Path:
    return SOVEREIGN_ROOT / "indexes" / corpus / "atlas" / name


def load_json(path: Path, default):
    if not path.exists():
        return default
    with open(path) as f:
        return json.load(f)


def extract_chunk_refs(obj) -> list[str]:
    """Walk a dict and pull out any `chunk_id` or nested ChunkRef sec_NNNN ids."""
    found: list[str] = []
    def walk(x):
        if isinstance(x, dict):
            cid = x.get("chunk_id")
            if isinstance(cid, str) and cid.startswith("sec_"):
                found.append(cid)
            for v in x.values():
                walk(v)
        elif isinstance(x, list):
            for v in x:
                walk(v)
    walk(obj)
    return sorted(set(found))


def section_range_ids(obj) -> list[str]:
    """Expand a {start: sec_0005, end: sec_0008} section_range into a list."""
    if not isinstance(obj, dict):
        return []
    sr = obj.get("section_range")
    if not isinstance(sr, dict):
        return []
    start = sr.get("start")
    end = sr.get("end")
    if not (isinstance(start, str) and isinstance(end, str)
            and start.startswith("sec_") and end.startswith("sec_")):
        return []
    try:
        a = int(start.split("_")[1])
        b = int(end.split("_")[1])
    except (IndexError, ValueError):
        return []
    return [f"sec_{i:04d}" for i in range(a, b + 1)]


def atoms_by_id(atoms: list[dict]) -> dict[str, dict]:
    out = {}
    for env in atoms:
        d = env.get("data", {})
        aid = d.get("id")
        if aid:
            out[aid] = {"atom_type": env.get("atom_type"), **d}
    return out


# ─── Query templates (deterministic path) ─────────────────────────────


def q_entity(e: dict) -> list[tuple[str, str]]:
    """Return [(query, classifier_hint), ...] for an Entity atom."""
    name = e.get("canonical_name") or e.get("label") or "this character"
    desc = (e.get("description") or "").strip()
    out = [
        (f"Who is {name}?", "entity.identity"),
        (f"Describe {name}'s role in the story.", "entity.role"),
    ]
    if desc and len(desc) > 20:
        # Snippet-based query — forces retrieval to match on specifics,
        # not just the name.
        snippet = desc.rstrip(".")[:120]
        out.append((f"Which character is described as: {snippet}?", "entity.description"))
    return out


def q_event(e: dict) -> list[tuple[str, str]]:
    desc = (e.get("description") or "this event").strip()
    return [
        (f"What happens when {desc[:140]}?", "event.what"),
        (f"Describe the scene where {desc[:140]}", "event.where"),
    ]


def q_relation(e: dict, atoms_idx: dict[str, dict]) -> list[tuple[str, str]]:
    label = e.get("label") or "relationship"
    parts = e.get("participants") or []
    names: list[str] = []
    for p in parts:
        a = atoms_idx.get(p)
        if a:
            names.append(a.get("canonical_name") or a.get("label") or p)
    if len(names) >= 2:
        return [
            (f"What is the relationship between {names[0]} and {names[1]}?",
             "relation.pair"),
            (f"How are {names[0]} and {names[1]} connected? ({label})",
             "relation.labeled"),
        ]
    return [(f"What is the {label}?", "relation.bare")]


def q_state(e: dict, atoms_idx: dict[str, dict]) -> list[tuple[str, str]]:
    entity_id = e.get("entity_id")
    label = e.get("label") or "state"
    a = atoms_idx.get(entity_id or "") or {}
    name = a.get("canonical_name") or a.get("label") or "the character"
    return [
        (f"How is {name} {label}?", "state.character"),
        (f"What is {name}'s emotional state in this scene?", "state.emotional"),
    ]


def q_claim(e: dict) -> list[tuple[str, str]]:
    content = (e.get("content") or "").strip()
    if not content:
        return []
    return [
        (f"What is argued about: {content[:160]}?", "claim.what"),
        (f"Is the claim valid: {content[:160]}", "claim.valid"),
    ]


def q_question(e: dict) -> list[tuple[str, str]]:
    content = (e.get("content") or "").strip()
    if not content:
        return []
    return [(content, "question.verbatim")]


def q_tension(cand: dict, atoms_idx: dict[str, dict]) -> list[tuple[str, str]]:
    s = atoms_idx.get(cand.get("source_atom") or "")
    t = atoms_idx.get(cand.get("target_atom") or "")
    shared = cand.get("shared_entity")
    sa = atoms_idx.get(shared or "") if shared else None
    ent_name = (sa or {}).get("canonical_name") if sa else None
    if s and t:
        s_label = s.get("content") or s.get("label") or s.get("canonical_name") or ""
        t_label = t.get("content") or t.get("label") or t.get("canonical_name") or ""
        frame = f" about {ent_name}" if ent_name else ""
        return [
            (f"How do these views conflict{frame}: '{s_label[:100]}' vs '{t_label[:100]}'?",
             "tension.pair"),
        ]
    return []


# ─── Ground-truth extraction ──────────────────────────────────────────


def atom_relevant_sections(atom: dict, atom_type: str) -> list[str]:
    secs: set[str] = set()
    # first_appearance.chunk_id
    fa = atom.get("first_appearance")
    if isinstance(fa, dict):
        cid = fa.get("chunk_id")
        if isinstance(cid, str) and cid.startswith("sec_"):
            secs.add(cid)
    # evidence: list[ChunkRef]
    for ev in atom.get("evidence") or []:
        if isinstance(ev, dict):
            cid = ev.get("chunk_id")
            if isinstance(cid, str) and cid.startswith("sec_"):
                secs.add(cid)
    # section_range
    secs.update(section_range_ids(atom))
    # raised_at for Questions
    ra = atom.get("raised_at")
    if isinstance(ra, dict):
        cid = ra.get("chunk_id")
        if isinstance(cid, str) and cid.startswith("sec_"):
            secs.add(cid)
    # fallback: whole-atom walk
    if not secs:
        secs.update(extract_chunk_refs(atom))
    return sorted(secs)


def tension_relevant_sections(cand: dict, atoms_idx: dict[str, dict]) -> list[str]:
    secs: set[str] = set()
    for role in ("source_atom", "target_atom"):
        aid = cand.get(role)
        a = atoms_idx.get(aid or "")
        if a:
            secs.update(atom_relevant_sections(a, a.get("atom_type", "")))
    return sorted(secs)


# ─── LLM paraphrase (optional) ────────────────────────────────────────


PARAPHRASE_PROMPT = """Rewrite the following question into 2 distinct natural research-style phrasings a reader of Brothers Karamazov might actually ask. Keep the same answer in mind — don't change what's being asked, only how. Output exactly 2 lines, no numbering, no preamble.

Question: {q}"""


def paraphrase(client: httpx.Client, model: str, q: str, timeout: float) -> list[str]:
    r = client.post(
        "/v1/chat/completions",
        json={
            "model": model,
            "messages": [{"role": "user", "content": PARAPHRASE_PROMPT.format(q=q)}],
            "temperature": 0.6,
            "max_tokens": 256,
        },
        timeout=timeout,
    )
    r.raise_for_status()
    text = r.json()["choices"][0]["message"]["content"].strip()
    # Strip any <think>...</think>, take last 2 non-empty lines.
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL).strip()
    lines = [ln.strip(" -*•\t") for ln in text.splitlines() if ln.strip()]
    return lines[:2]


# ─── Main ─────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--paraphrase", default=None,
                    help="Enable LLM paraphrase via this chat model id, e.g. Qwopus3.5-9B-v3.Q5_K_S")
    ap.add_argument("--sample-per-class", type=int, default=50,
                    help="Cap per (atom_type × class) to keep the set balanced")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--daemon", default=DAEMON_URL)
    args = ap.parse_args()

    rng = random.Random(args.seed)
    out_path = args.out or Path(f"queries-{args.corpus}.jsonl")

    atoms_file = load_json(atlas_path(args.corpus, "atoms.json"), {"atoms": []})
    edges_file = load_json(atlas_path(args.corpus, "edges.json"), {"edges": []})
    tension_file = load_json(
        atlas_path(args.corpus, "tension_candidates.json"),
        {"candidates": []},
    )

    atoms: list[dict] = atoms_file.get("atoms", [])
    tension_cands: list[dict] = tension_file.get("candidates", [])
    idx = atoms_by_id(atoms)
    print(f"loaded atoms={len(atoms)} edges={len(edges_file.get('edges', []))} "
          f"tension_candidates={len(tension_cands)}")

    # Bucket atoms by type to stratify.
    buckets: dict[str, list[dict]] = {}
    for env in atoms:
        buckets.setdefault(env.get("atom_type", "Unknown"), []).append(env)
    print("atom buckets:", {k: len(v) for k, v in buckets.items()})

    queries: list[dict] = []

    def emit(text: str, cls: str, sections: list[str], source_id: str):
        if not text or not sections:
            return
        queries.append({
            "query": text,
            "class": cls,
            "relevant_sections": sections,
            "source_id": source_id,
        })

    # Entity
    entities = buckets.get("Entity", [])
    rng.shuffle(entities)
    for env in entities[: args.sample_per_class]:
        a = {"atom_type": "Entity", **env["data"]}
        secs = atom_relevant_sections(a, "Entity")
        for q, cls in q_entity(a):
            emit(q, cls, secs, a.get("id", ""))

    # Event
    for env in (buckets.get("Event") or [])[: args.sample_per_class]:
        a = {"atom_type": "Event", **env["data"]}
        secs = atom_relevant_sections(a, "Event")
        for q, cls in q_event(a):
            emit(q, cls, secs, a.get("id", ""))

    # State
    for env in (buckets.get("State") or [])[: args.sample_per_class]:
        a = {"atom_type": "State", **env["data"]}
        secs = atom_relevant_sections(a, "State")
        for q, cls in q_state(a, idx):
            emit(q, cls, secs, a.get("id", ""))

    # Relation
    for env in (buckets.get("Relation") or [])[: args.sample_per_class]:
        a = {"atom_type": "Relation", **env["data"]}
        secs = atom_relevant_sections(a, "Relation")
        for q, cls in q_relation(a, idx):
            emit(q, cls, secs, a.get("id", ""))

    # Claim
    for env in (buckets.get("Claim") or [])[: args.sample_per_class]:
        a = {"atom_type": "Claim", **env["data"]}
        secs = atom_relevant_sections(a, "Claim")
        for q, cls in q_claim(a):
            emit(q, cls, secs, a.get("id", ""))

    # Question
    for env in (buckets.get("Question") or [])[: args.sample_per_class]:
        a = {"atom_type": "Question", **env["data"]}
        secs = atom_relevant_sections(a, "Question")
        for q, cls in q_question(a):
            emit(q, cls, secs, a.get("id", ""))

    # Tension candidates
    rng.shuffle(tension_cands)
    for cand in tension_cands[: args.sample_per_class]:
        secs = tension_relevant_sections(cand, idx)
        for q, cls in q_tension(cand, idx):
            emit(q, cls, secs, f"tension:{cand.get('source_atom')}→{cand.get('target_atom')}")

    print(f"queries (template path): {len(queries)}")

    # Optional paraphrase pass.
    if args.paraphrase:
        client = httpx.Client(base_url=args.daemon)
        t0 = time.time()
        expanded: list[dict] = []
        for q in tqdm(queries, desc="paraphrase"):
            expanded.append(q)
            try:
                variants = paraphrase(client, args.paraphrase, q["query"], timeout=120.0)
            except Exception as e:
                print(f"paraphrase failed for '{q['query'][:60]}': {e}",
                      file=sys.stderr)
                continue
            for v in variants:
                if v and v != q["query"]:
                    expanded.append({**q, "query": v, "class": q["class"] + ".paraphrase"})
        print(f"after paraphrase: {len(expanded)} queries "
              f"({time.time() - t0:.1f}s)")
        queries = expanded

    with open(out_path, "w") as f:
        for q in queries:
            f.write(json.dumps(q) + "\n")

    # Class distribution report.
    by_class: dict[str, int] = {}
    for q in queries:
        by_class[q["class"]] = by_class.get(q["class"], 0) + 1
    print("class distribution:")
    for cls, n in sorted(by_class.items()):
        print(f"  {cls:30s} {n:5d}")
    print(f"done: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
