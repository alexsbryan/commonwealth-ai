#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Convert the MoReBench public split into bench/moral scenario TOMLs.

Source: https://huggingface.co/datasets/morebench/morebench (CC-BY-4.0)
Paper:  MoReBench: Evaluating Procedural and Pluralistic Moral Reasoning
        in Language Models, More than Outcomes (arXiv:2510.16380)

The public split has 500 dilemmas, each with 20-47 weighted rubric
criteria tagged with one of five dimensions (identifying, logical
process, clear process, helpful outcome, harmless outcome; a handful
are tagged `other`). This script selects a deterministic stratified
subset and writes one TOML per scenario into scenarios/.

Determinism: scenario ids are sha1(DILEMMA)[:10] — identity from
content, never a row index. Within each stratum, rows are ordered by
that hash and the first k are taken, so re-running the converter on
the same upstream data reproduces the same bank byte-for-byte.

Usage:
    python3 convert_morebench.py [--parquet PATH] [--out scenarios/] [--per-stratum-json '{"..."}']

With no --parquet the script downloads the public split from the
HuggingFace parquet endpoint (no token required; the dataset is
public and ungated).
"""

import argparse
import ast
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

PARQUET_URL = (
    "https://huggingface.co/api/datasets/morebench/morebench/"
    "parquet/morebench_public/test/0.parquet"
)

# Stratified take: proportional to the split's own composition
# (daily_dilemmas 200, ai_risk_dilemmas 200, expert_* 100). Sized at
# 56 scenarios (~1,300 criteria) so the thinnest dimension
# (harmless outcome, ~7% of upstream criteria) still lands ~90
# criteria — enough for a per-dimension delta to clear its own CI.
DEFAULT_TAKE = {
    "daily_dilemmas": 18,
    "ai_risk_dilemmas": 18,
    "expert_written_ethic_bowl": 8,
    "expert_written_ethic_unwrapped": 6,
    "expert_written_literature": 4,
    "expert_written_collab": 2,
}

KNOWN_DIMENSIONS = {
    "identifying",
    "logical process",
    "clear process",
    "helpful outcome",
    "harmless outcome",
    "other",
}


def tstr(s: str) -> str:
    """TOML basic string via JSON escaping (a valid subset)."""
    return json.dumps(s, ensure_ascii=False)


def scenario_toml(row: dict, rubric: list, sid: str) -> str:
    lines = [
        "# Converted from the MoReBench public split (CC-BY-4.0).",
        "# Do not hand-edit criteria text: regenerate via convert_morebench.py.",
        "",
        "[scenario]",
        f"id = {tstr(sid)}",
        'source = "morebench_public"',
        f"dilemma_source = {tstr(row['DILEMMA_SOURCE'])}",
        f"dilemma_type = {tstr(row['DILEMMA_TYPE'])}",
        f"role_domain = {tstr(row['ROLE_DOMAIN'])}",
        f"context = {tstr(row.get('CONTEXT') or '')}",
        "",
        "[dilemma]",
        f"prompt = {tstr(row['DILEMMA'])}",
        "",
    ]
    for c in rubric:
        dim = c["annotations"]["rubric_dimension"]
        if dim not in KNOWN_DIMENSIONS:
            print(f"  warn: {sid}: unknown dimension {dim!r}, keeping verbatim")
        lines.extend(
            [
                "[[criteria]]",
                f"id = {tstr(c['id'][:8])}",
                f"text = {tstr(c['title'])}",
                f"dimension = {tstr(dim)}",
                f"weight = {c['weight']}",
                "",
            ]
        )
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--parquet", help="local parquet path (else download)")
    ap.add_argument("--out", default=None, help="output dir (default: scenarios/ next to this script)")
    ap.add_argument("--per-stratum-json", help="JSON object overriding the per-stratum take counts")
    args = ap.parse_args()

    out_dir = Path(args.out) if args.out else Path(__file__).parent / "scenarios"
    take = dict(DEFAULT_TAKE)
    if args.per_stratum_json:
        take.update(json.loads(args.per_stratum_json))

    if args.parquet:
        parquet_path = Path(args.parquet)
    else:
        parquet_path = Path("/tmp/morebench_public.parquet")
        if not parquet_path.exists():
            print(f"downloading {PARQUET_URL}")
            urllib.request.urlretrieve(PARQUET_URL, parquet_path)

    try:
        import pyarrow.parquet as pq
    except ImportError:
        print("error: pyarrow required (pip install pyarrow)", file=sys.stderr)
        return 2

    rows = pq.read_table(parquet_path).to_pylist()
    print(f"loaded {len(rows)} rows from {parquet_path}")

    by_stratum: dict = {}
    for row in rows:
        sid = "mb-" + hashlib.sha1(row["DILEMMA"].encode("utf-8")).hexdigest()[:10]
        by_stratum.setdefault(row["DILEMMA_SOURCE"], []).append((sid, row))

    out_dir.mkdir(parents=True, exist_ok=True)
    written = 0
    for stratum, k in sorted(take.items()):
        pool = sorted(by_stratum.get(stratum, []), key=lambda t: t[0])
        if len(pool) < k:
            print(f"  warn: stratum {stratum} has {len(pool)} rows, wanted {k}")
        # Alternate advisor/agent roles within the stratum where both
        # exist, so the subset carries both role framings.
        advisors = [t for t in pool if t[1]["ROLE_DOMAIN"] == "ai_advisor"]
        agents = [t for t in pool if t[1]["ROLE_DOMAIN"] == "ai_agent"]
        picked, ai, ag = [], 0, 0
        for i in range(min(k, len(pool))):
            if i % 2 == 0 and ai < len(advisors):
                picked.append(advisors[ai]); ai += 1
            elif ag < len(agents):
                picked.append(agents[ag]); ag += 1
            elif ai < len(advisors):
                picked.append(advisors[ai]); ai += 1
        for sid, row in picked:
            rubric = ast.literal_eval(row["RUBRIC"])
            if not rubric:
                print(f"  warn: {sid} has empty rubric, skipping")
                continue
            (out_dir / f"{sid}.toml").write_text(scenario_toml(row, rubric, sid), encoding="utf-8")
            written += 1
    print(f"wrote {written} scenarios to {out_dir}")
    return 0 if written else 1


if __name__ == "__main__":
    sys.exit(main())
