#!/usr/bin/env python3
"""
raptor_faithfulness_probe.py — validate the embedded hypotheses behind the
RAPTOR-fabrication fix BEFORE committing to the program.

Context: the RAPTOR cluster-summarizer (sovereign-tools/src/raptor_atlas.rs:668)
produced "the Russian agent Vladimir" for Conrad's *The Secret Agent*, where
"Russian" is 0x in the source. The fix program assumed (A) lowering temperature
+ a faithfulness instruction reduces fabrication. This harness FALSIFIES the
sub-hypotheses instead of assuming them:

  H1 fabrication is driven by TEMPERATURE (0.2)        -> temp {0.0, 0.2}
  H2 fabrication is driven by PROMPT FRAMING           -> prompt {current, faithful}
  H3 temp 0 DEGRADES summary quality/recall            -> quality proxy + samples
  H4 fabrication is SYSTEMATIC, not the one anecdote    -> many clusters, a rate

Method: a 2x2 factorial (temp x prompt) x N samples over FIXED clusters
(paragraph windows from the clean chaos source, incl. the Vladimir/embassy
scene). The summarizer prompt is copied VERBATIM from raptor_atlas.rs:668-679.
Each summary is scored by a separate faithfulness JUDGE (held-constant model)
that lists claims NOT stated/entailed by that summary's own input passages.
Fabrication rate = summaries-with-an-unsupported-claim / N, per cell.

Fidelity caveats (recorded, not hidden): this hits /v1/chat/completions rather
than the internal CompletionRequest path, so it does NOT apply raptor_atlas's
lark grammar (which only forbids the '"' byte -> irrelevant to factual content)
nor think_budget=0 exactly. The load-bearing levers (verbatim prompt, temp,
model, max_tokens=500) are reproduced. Run with --model matching the slot whose
fabrication you care about; the LEVER conclusions (does temp/prompt move the
rate?) hold regardless of absolute model tendency.
"""
import argparse, json, os, re, sys, time, urllib.request, urllib.error

# ── The VERBATIM summarizer prompt (raptor_atlas.rs:668-679, Narrative cue). ──
# Kept byte-faithful so the experiment exercises the real task framing.
NARRATIVE_CUE = "scene-level summary: who is present, what happens, what shifts"
DOC_TYPE = "narrative"

def build_summarizer_prompt(passages: str, variant: str) -> str:
    base = (
        f"You are summarizing a group of related passages from a {DOC_TYPE} document.\n"
        f"Produce a {NARRATIVE_CUE}. The summary is a paraphrase — do NOT include any quotation marks "
        f"or verbatim quotations; we hold the source separately. Also list the primary entities "
        f"(characters, organizations, places, key concepts) by their canonical names as they "
        f"appear in the passages.\n\n"
    )
    if variant == "faithful":
        # The ONLY change under test: a faithfulness floor. Isolates H2.
        base = (
            f"You are summarizing a group of related passages from a {DOC_TYPE} document.\n"
            f"Produce a section-level summary of ONLY what the passages explicitly state. "
            f"Do NOT infer, speculate, or add any name, nationality, location, relationship, "
            f"motive, or fact that is not directly stated in the passages. If the passages do "
            f"not state something, omit it — do not fill the gap from outside knowledge. "
            f"The summary is a paraphrase — do NOT include any quotation marks or verbatim "
            f"quotations. Also list the primary entities (characters, organizations, places, key "
            f"concepts) by their canonical names as they appear in the passages.\n\n"
        )
    return (
        base
        + 'Respond with a single JSON object only:\n'
        + '{"summary": "<2-4 sentences, no quote marks>", "primary_entities": ["Name1", "Name2"]}\n\n'
        + f"Passages:\n{passages}\n\nJSON:"
    )

JUDGE_PROMPT = """\
You are checking a SUMMARY for MATERIAL FABRICATION against its SOURCE PASSAGES.

A MATERIAL FABRICATION is a SPECIFIC, CHECKABLE FACT the summary asserts as \
established that the source passages do NOT state or directly entail: a proper \
name, a nationality or country, a place, a number/date/age, a family or \
organizational relationship, or a concrete event/action. The test: would a \
reader come away believing a specific fact about the subject that the passages \
do not actually support?

Do NOT flag (these are legitimate abstraction, NOT fabrication):
- rewording/paraphrase of something the passages do say (e.g. "in charge of" -> "oversees")
- general narrative framing or description ("a young boy", "an unnamed narrator", "tense scene")
- hedged or interpretive language about tone, mood, or what a passage conveys
- expanding an abbreviation that the passages make obvious
- omitting detail, or summarizing at a high level

ONLY flag a claim that introduces a NEW, SPECIFIC, FALSE-OR-UNSUPPORTED fact. \
Example to flag: source says "a foreign embassy" but summary says "the Russian \
embassy" -> flag "Russian" (nationality the source withholds). Example NOT to \
flag: source says "the boy" and summary says "the young boy" -> do not flag.
General world knowledge does NOT count as source support; only the passages do.

SOURCE PASSAGES:
\"\"\"
{passages}
\"\"\"

SUMMARY:
\"\"\"
{summary}
\"\"\"

Respond with a single JSON object only:
{{"material_fabrications": ["<each specific unsupported fact, named>"], "verdict": "faithful" | "fabricated"}}
"verdict" is "fabricated" if and only if "material_fabrications" is non-empty.
JSON:"""


def http_chat(base_url, model, prompt, temperature, max_tokens, timeout=180):
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": temperature,
        "max_tokens": max_tokens,
    }).encode()
    req = urllib.request.Request(
        f"{base_url.rstrip('/')}/v1/chat/completions",
        data=body, headers={"Content-Type": "application/json"}, method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            d = json.load(r)
        return d["choices"][0]["message"]["content"]
    except (urllib.error.URLError, KeyError, json.JSONDecodeError, TimeoutError) as e:
        return f"__ERROR__ {e}"


def strip_think(s: str) -> str:
    return re.sub(r"<think>.*?</think>", "", s, flags=re.DOTALL).strip()


def parse_json_obj(s: str):
    s = strip_think(s)
    m = re.search(r"\{.*\}", s, flags=re.DOTALL)
    if not m:
        return None
    try:
        return json.loads(m.group(0))
    except json.JSONDecodeError:
        return None


def extract_clusters(text: str, paras_per: int, max_clusters: int):
    """Paragraph-window clusters across the document; GUARANTEE the
    Vladimir/embassy scene (the known fabrication case) is cluster 0."""
    paras = [p.strip() for p in re.split(r"\n\s*\n", text) if len(p.strip()) > 120]
    clusters = []
    # Anchor 0: the Vladimir embassy-INTERVIEW scene (the fabrication witness).
    # Prefer a paragraph with Vladimir's substantive demands — the part the
    # original RAPTOR summary covered ("demands an anarchist outrage"), where
    # the "Russian agent" inference arises — then the embassy anchor, then any
    # Vladimir mention. Widen the window to span the interview, mirroring a real
    # (embedding-clustered) RAPTOR leaf rather than a thin consecutive slice.
    sub = re.compile(r"outrage|explosion|science|Greenwich|observatory|astronom|anarch", re.I)
    anchor = next((i for i, p in enumerate(paras) if "Vladimir" in p and sub.search(p)), None)
    if anchor is None:
        anchor = next((i for i, p in enumerate(paras)
                       if "Vladimir" in p and ("embassy" in p.lower() or "Secretary" in p)), None)
    if anchor is None:
        anchor = next((i for i, p in enumerate(paras) if "Vladimir" in p), 0)
    vlad_w = paras_per + 5
    lo = max(0, anchor - 2)
    clusters.append(("vladimir-embassy", "\n\n".join(paras[lo:lo + vlad_w])))
    # Remaining: evenly-strided windows across the rest (fixed, reproducible).
    stride = max(paras_per, len(paras) // (max_clusters))
    for ci, start in enumerate(range(0, len(paras) - paras_per, stride)):
        if len(clusters) >= max_clusters:
            break
        win = "\n\n".join(paras[start:start + paras_per])
        if win and not any(win == c[1] for c in clusters):
            clusters.append((f"window-{start}", win))
    return clusters[:max_clusters]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", default="http://localhost:9741")
    ap.add_argument("--model", default="primary", help="summarizer slot under test")
    ap.add_argument("--judge-model", default="fast", help="held-constant faithfulness judge")
    ap.add_argument("--source", default=os.path.expanduser("~/.sovereign/bench-corpora/chaos-secret-agent/secret-agent.txt"))
    ap.add_argument("--n", type=int, default=5, help="samples per cell")
    ap.add_argument("--clusters", type=int, default=8)
    ap.add_argument("--paras-per-cluster", type=int, default=5)
    ap.add_argument("--temps", default="0.0,0.2")
    ap.add_argument("--variants", default="current,faithful")
    ap.add_argument("--out", default="target/ci-bench/raptor-faithfulness/probe.jsonl")
    args = ap.parse_args()

    text = open(args.source, encoding="utf-8", errors="ignore").read()
    clusters = extract_clusters(text, args.paras_per_cluster, args.clusters)
    temps = [float(t) for t in args.temps.split(",")]
    variants = args.variants.split(",")
    import os
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    fout = open(args.out, "w")

    print(f"[probe] model={args.model} judge={args.judge_model} clusters={len(clusters)} "
          f"temps={temps} variants={variants} n={args.n}")
    print(f"[probe] cluster 0 = {clusters[0][0]} (fabrication witness)")

    # cell -> {"total","fabricated","russian_hits","russian_total","summ_chars"}
    cells = {}
    for variant in variants:
        for temp in temps:
            key = f"{variant}@t{temp}"
            cells[key] = {"total": 0, "fabricated": 0, "russian_hits": 0,
                          "russian_total": 0, "summ_chars": 0, "judge_fail": 0}
            for (cid, passages) in clusters:
                sprompt = build_summarizer_prompt(passages, variant)
                for s in range(args.n):
                    raw = http_chat(args.base_url, args.model, sprompt, temp, 500)
                    obj = parse_json_obj(raw)
                    summary = (obj or {}).get("summary", "") if obj else ""
                    if not summary:
                        print(f"  [{key}] {cid} s{s}: __no-summary__ ({raw[:80]!r})")
                        continue
                    # Faithfulness judge (held-constant model, temp 0).
                    jraw = http_chat(args.base_url, args.judge_model,
                                     JUDGE_PROMPT.format(passages=passages, summary=summary), 0.0, 400)
                    jobj = parse_json_obj(jraw)
                    if jobj is None:
                        cells[key]["judge_fail"] += 1
                        verdict, unsupported = "judge_error", []
                    else:
                        unsupported = jobj.get("material_fabrications", []) or []
                        verdict = jobj.get("verdict", "faithful" if not unsupported else "fabricated")
                    c = cells[key]
                    c["total"] += 1
                    c["summ_chars"] += len(summary)
                    if verdict == "fabricated":
                        c["fabricated"] += 1
                    # Targeted probe: the specific known fabrication (only the
                    # Vladimir cluster can fabricate "Russian" — source is 0x).
                    if cid == "vladimir-embassy":
                        c["russian_total"] += 1
                        if re.search(r"russia", summary, re.I):
                            c["russian_hits"] += 1
                    fout.write(json.dumps({
                        "cell": key, "variant": variant, "temp": temp, "cluster": cid,
                        "sample": s, "summary": summary, "verdict": verdict,
                        "unsupported": unsupported,
                    }) + "\n")
                    fout.flush()
                    print(f"  [{key}] {cid} s{s}: {verdict}"
                          + (f" | unsupported={unsupported}" if unsupported else "")
                          + (" | RUSSIAN" if cid == 'vladimir-embassy' and re.search(r'russia', summary, re.I) else ""))
    fout.close()

    # ── 2x2 verdict table ────────────────────────────────────────────────
    print("\n" + "=" * 78)
    print(" RAPTOR faithfulness probe — fabrication rate by (prompt x temp)")
    print("=" * 78)
    print(f"  {'cell':18} {'fab_rate':>10} {'russian':>10} {'avg_chars':>10} {'judge_err':>10}")
    for key, c in cells.items():
        if c["total"] == 0:
            print(f"  {key:18} {'(no data)':>10}")
            continue
        fab = c["fabricated"] / c["total"]
        rus = f"{c['russian_hits']}/{c['russian_total']}" if c["russian_total"] else "-"
        print(f"  {key:18} {fab:>10.2f} {rus:>10} {c['summ_chars']//c['total']:>10} {c['judge_fail']:>10}")
    print("-" * 78)
    print(" Read: does fab_rate move with TEMP (H1) or with PROMPT (H2)? "
          "Does avg_chars/quality drop at temp 0 (H3)? Is fab_rate>0 broadly (H4)?")
    print(f" Full transcripts: {args.out}")


if __name__ == "__main__":
    main()
