#!/usr/bin/env python3
"""t4a-retrace.py — the T3C forensics' method applied to NEW flights
(order deep-research-t4a, pre-registered in
research/deep-research/adversarial/pre-registration.md).

Re-measures four deterministic surfaces on every flight under a root:

  (1) untraced-but-present — the forensics' method verbatim: for every
      gap-list witness record whose reason contains "untraced: ", parse
      the figure list and count each figure's presence in the flight's
      OWN union. The union is the MERGED window view (amendment 2b,
      pre-registered): the round-1 estate window reconstructed from
      survey-*.json (the loop builds it at mod.rs:1091-1114 and pushes it
      before the fetch window) joined with the persisted
      evidence-window-*.json, first-wins source_url dedup (mod.rs:1132),
      capped at the charter's evidence_window_max_chunks. The persisted
      windows alone are a NARROWER view than the audit's — drb-62 c9's
      "2022" figures verified against the estate chunk the gate saw, but
      were absent from the persisted-only union. PRIMARY:
      case-insensitive substring (the method that produced 127/161 = 79%).
      SECONDARY: whole-token (the same tokenizer applied to the union).
  (2) tails — count of "[Source:" in the flight's rendered report.md
      (target 0; the render pins !report.contains("[Source:")).
  (3) structured citations — every [passed] claim in verdict-set.json
      carries non-empty evidence_ids AND citations[] (the typed
      channel).
  (4) constitution — the [passed] claims' figure tokens (measurement
      port of figure_tokens, deep_research/mod.rs:473) all present in
      the flight union (substring presence — the permissive side; any
      absent-by-substring figure is a true violation), plus no untraced
      witness record on a [passed] claim id.

Layouts:
  battery (arms root):  <root>/loop/<id>/dr-<ts>/   (loop arm flights)
  drb (re-flight root): <root>/drb-<tid>/dr-<ts>/   (DRB driver layout;
                          local/hybrid arms — amendment 2, see pre-registration)

Any residual is NAMED (flight, claim id, tokens) — never smoothed.

Usage:
  t4a-retrace.py --root <dir> [--out <file.json>] [--label <name>]
"""
import argparse
import json
import pathlib
import re
import sys

FIGURE_RE = re.compile(r"[0-9$%.:/,]+")
UNTRACED_RE = re.compile(r"untraced:\s*(.+)", re.IGNORECASE)
HANDLE_RE = re.compile(r"\[Source:\s*([^\]]+)\]")


def strip_citation_spans(text: str) -> str:
    """Port of strip_citation_spans (containment.rs:127): drop
    `[Source: …]` spans (terminated at ']', else to end-of-line), keep
    the rest. Amendment 2c — the constitution leg tokenizes
    strip_citation_spans(claim), matching missing_claim_figures
    (containment.rs:262-268): the gate never sees handle digits as claim
    figures, and the measurement must not either."""
    out = []
    rest = text
    while True:
        start = rest.find("[Source:")
        if start < 0:
            out.append(rest)
            break
        out.append(rest[:start])
        after = rest[start:]
        end = after.find("]")
        if end >= 0:
            rest = after[end + 1 :]
        else:
            nl = after.find("\n")
            rest = after[nl:] if nl >= 0 else ""
    return "".join(out)
# The matcher's STOP list, verbatim from value_presence.rs.
STOP = {
    "mr", "mrs", "miss", "ms", "the", "of", "a", "an", "and", "sir", "dr",
    "comrade", "chief", "inspector", "lady", "lord", "saint", "st",
}


def figure_tokens(text: str):
    """Measurement port of figure_tokens (deep_research/mod.rs:473):
    maximal runs of digits and $ % . : / , ; trailing '.', ',' popped."""
    return [t.rstrip(".,") for t in FIGURE_RE.findall(text) if t.rstrip(".,")]


def is_heading_shaped(line: str) -> bool:
    """Port of is_heading_shaped (containment.rs): <=80 chars, no
    sentence-final punctuation, no continuation."""
    s = line.strip()
    if len(s) > 80:
        return False
    if s.endswith((".", "?", "!")):
        return False
    return True


def appears_in_body(specific: str, evidence: str) -> bool:
    """Port of appears_in_body INCLUDING the t4a provenance-aware heading
    exception (containment.rs:192-211): heading-shaped lines count when the
    specific carries figure tokens."""
    low = specific.lower()
    for line in evidence.splitlines():
        if is_heading_shaped(line):
            if figure_tokens(specific) and low in line.lower():
                return True
        elif low in line.lower():
            return True
    return False


def value_present_in_chunks(value: str, chunks) -> bool:
    """Exact port of value_present_in_chunks (value_presence.rs:152):
    verbatim multi-word path + significant-word path. The significance
    floor (per-split-part >= 2 chars) is what rejects decimal figures
    like "9.5" (split into 1-char halves) and single digits like "1"."""
    hay = " ".join(chunks).lower().split()
    nval = value.lower().split()
    if len([w for w in nval if w]) >= 2 and " ".join(hay).find(" ".join(nval)) >= 0:
        return True
    sig = [
        w.lower()
        for w in re.split(r"[^0-9A-Za-z]+", value)
        if len(w) >= 2 and w.lower() not in STOP
    ]
    if not sig:
        return False
    return all(any(h == w for h in hay) for w in sig)


def present_by_discipline(fig: str, ref_text: str) -> bool:
    """The witness's present closure: matcher discipline AND line presence."""
    return value_present_in_chunks(fig, [ref_text]) and appears_in_body(fig, ref_text)


def parse_untraced(reason: str):
    """Parse the recorded "untraced: 68, 0.5469" figure list."""
    if not reason:
        return []
    m = UNTRACED_RE.search(reason)
    if not m:
        return []
    return [f.strip() for f in m.group(1).split(",") if f.strip()]


def _landed(d: pathlib.Path) -> bool:
    """Amendment 2a — the landed-flight gate: a flight dir counts only when
    it carries verdict-set.json (written at flight end). Aborted dirs left
    by failed/re-run flights have gap-lists but no verdict set and must not
    pollute the pooled tallies."""
    return (d / "verdict-set.json").is_file()


def flight_dirs(root: pathlib.Path):
    if (root / "loop").is_dir():
        # battery layout: <root>/loop/<pair>/dr-<ts>/   (loop arm flights)
        for pair in sorted((root / "loop").iterdir()):
            if not pair.is_dir():
                continue
            for d in sorted(pair.glob("dr-*")):
                if _landed(d):
                    yield pair.name, d
    elif any(p.is_dir() and p.name.startswith("drb-") for p in root.iterdir()):
        # DRB driver layout (t2b..t4a, frozen runs included — amendment 2):
        # <root>/drb-<tid>/dr-<ts>/   (local/hybrid arm flights)
        for drb in sorted(root.glob("drb-*")):
            if not drb.is_dir():
                continue
            for d in sorted(drb.glob("dr-*")):
                if _landed(d):
                    yield drb.name, d
    else:
        for d in sorted(root.glob("dr-*")):
            if _landed(d):
                yield d.parent.name, d


def merged_view(flight: pathlib.Path):
    """Amendment 2b — the MERGED window view (the audit's actual surface).

    The audit does not read the persisted evidence-window-*.json alone:
    round 1 pushes the ESTATE window (estate_window,
    deep_research/mod.rs:1091-1114 — id `estate-{i+1}` per searched query
    index i, source_url = hit url else `estate:{corpus_id}:{chunk_id}`,
    content = hit content else snippet) BEFORE the round's fetch window,
    and merge_windows (mod.rs:1132) dedups by source_url FIRST-WINS — so
    for URLs the corpus hits covered, the gate sees the ESTATE chunk, not
    the persisted ev-N fetch chunk. The persisted-only view is NARROWER
    than the gate's. This function reconstructs the estate window from
    survey-*.json (the same construction the code uses) and merges it
    with the persisted windows — estate first, first-wins URL dedup,
    capped at the charter's evidence_window_max_chunks (the cap never bit
    on any measured flight: max merged unique = 16 < 20 — recorded in the
    execution record). Returns (union_text, ref_map id -> [contents]).
    """
    chunks = []  # (id, source_url, content) in push order

    # The estate window: the loop builds it only at round 1, but iterate
    # every survey-*.json so the view is robust to layout changes.
    for s in sorted(flight.glob("survey-*.json")):
        try:
            data = json.loads(s.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        for i, q in enumerate(data.get("searched", [])):
            for hit in q.get("hits", []):
                loc = hit.get("url") or "estate:{}:{}".format(
                    hit.get("corpus_id"), hit.get("chunk_id")
                )
                chunks.append(
                    (
                        "estate-{}".format(i + 1),
                        loc,
                        hit.get("content") or hit.get("snippet") or "",
                    )
                )

    # The persisted fetch windows (the only windows that survive to disk).
    for w in sorted(flight.glob("evidence-window-*.json")):
        try:
            data = json.loads(w.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        for c in data.get("chunks", []):
            chunks.append((c.get("id"), c.get("source_url"), c.get("content", "")))

    # The merge: first-wins by source_url, capped (merge_windows mod.rs:1132).
    cap = 20
    try:
        charter = json.loads((flight / "charter.json").read_text())
        cap = charter.get("charter", {}).get("evidence_window_max_chunks", cap)
    except (OSError, json.JSONDecodeError):
        pass
    merged = []
    seen = set()
    for cid, url, content in chunks:
        if not url or url in seen:
            continue
        seen.add(url)
        if len(merged) >= cap:
            continue
        merged.append((cid, url, content))

    union = "\n".join(c for _, _, c in merged)
    ref_map = {}
    for cid, _, content in merged:
        ref_map.setdefault(cid, []).append(content)
    return union, ref_map


def present_substring(token: str, union: str) -> bool:
    return token.lower() in union.lower()


def present_token(token: str, union: str) -> bool:
    """Whole-token: the union tokenized by the SAME tokenizer; a figure
    is present iff it appears as a whole figure-token."""
    return token in figure_tokens(union)


def retrace_flight(pair_id: str, flight: pathlib.Path) -> dict:
    rec = {
        "flight": str(flight),
        "pair": pair_id,
        "untraced_claims_checked": 0,
        "untraced_claims_with_present": 0,
        "untraced_tokens_total": 0,
        "untraced_tokens_present_substring": 0,
        "untraced_tokens_present_token": 0,
        "ref_real_leak": 0,          # present by the gate's discipline in referenced chunks
        "ref_matcher_significance": 0,  # substring-present but below the significance floor
        "ref_genuine_absent": 0,     # absent even as substring from referenced chunks
        "ref_heading_class": 0,      # present in referenced chunks only on heading-shaped lines
        "ref_scoped_records": 0,     # records with resolvable handles
        "residual": [],  # named: {claim_id, tokens_present}
        "tails_in_report": None,  # None = no report.md on disk
        "passed_claims": 0,
        "passed_with_untraced_figures": 0,
        "passed_with_recorded_untraced": 0,
        "passed_missing_structured_citations": 0,
        "constitution_violations": [],  # named: {claim_id, tokens_absent}
    }

    # Amendment 2b — the merged window view: the audit's actual surface
    # (estate window + persisted fetch windows, first-wins URL dedup).
    union, ref_map = merged_view(flight)

    # --- (1) untraced-but-present, the forensics' method verbatim -----
    recorded_untraced = set()  # claim ids with a recorded untraced list
    recorded_untraced_texts = set()  # exact claim texts (text-matched check)
    for gap in sorted(flight.glob("gap-list-*.json")):
        try:
            data = json.loads(gap.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        for c in data.get("claims", []):
            witness = c.get("witness") or {}
            if not witness.get("ran"):
                continue
            tokens = parse_untraced(witness.get("reason"))
            if not tokens:
                continue
            rec["untraced_claims_checked"] += 1
            rec["untraced_tokens_total"] += len(tokens)
            present_s = [t for t in tokens if present_substring(t, union)]
            present_t = [t for t in tokens if present_token(t, union)]
            rec["untraced_tokens_present_substring"] += len(present_s)
            rec["untraced_tokens_present_token"] += len(present_t)
            if present_s:
                rec["untraced_claims_with_present"] += 1
                rec["residual"].append(
                    {"claim_id": c.get("id"), "tokens_present": present_s}
                )
            recorded_untraced.add(c.get("id"))
            recorded_untraced_texts.add(c.get("text", ""))

            # ref-scoped presence pass (named instrument amendment):
            # resolve the claim's handles to referenced chunk contents
            # and class each recorded figure by the gate's own discipline.
            handles = [h.strip() for h in HANDLE_RE.findall(c.get("text", "") or "")]
            ref_text = "\n".join(
                seg for h in handles for seg in ref_map.get(h, [])
            )
            if not handles:
                continue  # no handle: not classable by reference (refused earlier)
            rec["ref_scoped_records"] += 1
            for fig in tokens:
                if present_by_discipline(fig, ref_text):
                    rec["ref_real_leak"] += 1
                elif present_substring(fig, ref_text):
                    hs_lines = [
                        l for l in ref_text.splitlines()
                        if is_heading_shaped(l) and fig.lower() in l.lower()
                    ]
                    if hs_lines:
                        rec["ref_heading_class"] += 1
                    else:
                        rec["ref_matcher_significance"] += 1
                else:
                    rec["ref_genuine_absent"] += 1

    # --- (2) tails in the rendered report -----------------------------
    report = flight / "report.md"
    if report.is_file():
        rec["tails_in_report"] = report.read_text(errors="replace").count("[Source:")
    else:
        rec["tails_in_report"] = None

    # --- (3)+(4) the verdict set: structured citations + constitution --
    vs = flight / "verdict-set.json"
    if vs.is_file():
        try:
            data = json.loads(vs.read_text())
        except (OSError, json.JSONDecodeError):
            data = {}
        for c in data.get("claims", []):
            if c.get("verdict") != "passed":
                continue
            rec["passed_claims"] += 1
            # recorded-side check, TEXT-MATCHED (named instrument
            # amendment): the round-scoped gap-list ids are per-round
            # indices, not identities — a recorded untraced counts
            # against a passed claim only when the claim TEXTS match.
            if c.get("text", "") in recorded_untraced_texts:
                rec["passed_with_recorded_untraced"] += 1
                rec["constitution_violations"].append(
                    {"claim_id": c.get("id"), "recorded_untraced": True}
                )
            if not (c.get("evidence_ids") and c.get("citations")):
                rec["passed_missing_structured_citations"] += 1
            absent = [
                t
                for t in figure_tokens(strip_citation_spans(c.get("text") or ""))
                if not present_substring(t, union)
            ]
            if absent:
                rec["passed_with_untraced_figures"] += 1
                rec["constitution_violations"].append(
                    {"claim_id": c.get("id"), "tokens_absent": absent}
                )
    return rec


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", required=True, help="flight root (arms or drb layout)")
    ap.add_argument("--out", help="write the JSON report here (default: stdout)")
    ap.add_argument("--label", default="", help="arm label for the report")
    args = ap.parse_args()

    root = pathlib.Path(args.root)
    if not root.is_dir():
        print(f"t4a-retrace: root not a directory: {root}", file=sys.stderr)
        return 2

    flights = list(flight_dirs(root))
    if not flights:
        print(f"t4a-retrace: no flights under {root}", file=sys.stderr)
        return 2

    per = [retrace_flight(pair, f) for pair, f in flights]

    pooled = {
        "label": args.label,
        "root": str(root),
        "flights": len(per),
        "untraced_claims_checked": sum(r["untraced_claims_checked"] for r in per),
        "untraced_claims_with_present": sum(
            r["untraced_claims_with_present"] for r in per
        ),
        "untraced_tokens_total": sum(r["untraced_tokens_total"] for r in per),
        "untraced_tokens_present_substring": sum(
            r["untraced_tokens_present_substring"] for r in per
        ),
        "untraced_tokens_present_token": sum(
            r["untraced_tokens_present_token"] for r in per
        ),
        "ref_scoped_records": sum(r["ref_scoped_records"] for r in per),
        "ref_real_leak": sum(r["ref_real_leak"] for r in per),
        "ref_matcher_significance": sum(r["ref_matcher_significance"] for r in per),
        "ref_genuine_absent": sum(r["ref_genuine_absent"] for r in per),
        "ref_heading_class": sum(r["ref_heading_class"] for r in per),
        "tails_total": sum(
            (r["tails_in_report"] or 0) for r in per if r["tails_in_report"] is not None
        ),
        "flights_with_tails": sum(
            1 for r in per if (r["tails_in_report"] or 0) > 0
        ),
        "flights_without_report": sum(1 for r in per if r["tails_in_report"] is None),
        "passed_claims": sum(r["passed_claims"] for r in per),
        "passed_with_untraced_figures": sum(
            r["passed_with_untraced_figures"] for r in per
        ),
        "passed_with_recorded_untraced": sum(
            r["passed_with_recorded_untraced"] for r in per
        ),
        "passed_missing_structured_citations": sum(
            r["passed_missing_structured_citations"] for r in per
        ),
        "residual": [
            {"flight": r["flight"], "pair": r["pair"], "items": r["residual"]}
            for r in per
            if r["residual"]
        ],
        "constitution_violations": [
            {"flight": r["flight"], "pair": r["pair"], "items": r["constitution_violations"]}
            for r in per
            if r["constitution_violations"]
        ],
    }

    report = {"pooled": pooled, "per_flight": per}
    if args.out:
        pathlib.Path(args.out).write_text(json.dumps(report, indent=2))
        print(f"t4a-retrace: {len(per)} flights -> {args.out}")
    else:
        print(json.dumps(report, indent=2))

    # The gate lines (done-when (a)/(b)/(d) of the order) — a nonzero
    # target line is a named residual, never smoothed.
    print("---")
    print(f"untraced-but-present: {pooled['untraced_claims_with_present']}/{pooled['untraced_claims_checked']} claims, "
          f"{pooled['untraced_tokens_present_substring']}/{pooled['untraced_tokens_total']} tokens (substring; "
          f"whole-token {pooled['untraced_tokens_present_token']})")
    print(f"ref-scoped class (the gate's own view): real_leak {pooled['ref_real_leak']} | "
          f"matcher-significance {pooled['ref_matcher_significance']} | "
          f"genuine_absent {pooled['ref_genuine_absent']} | heading_class {pooled['ref_heading_class']} "
          f"over {pooled['ref_scoped_records']} records")
    print(f"tails in rendered reports: {pooled['tails_total']} on {pooled['flights_with_tails']} flights")
    print(f"constitution: passed {pooled['passed_claims']}; with untraced figures {pooled['passed_with_untraced_figures']}; "
          f"with recorded untraced {pooled['passed_with_recorded_untraced']}; "
          f"missing structured citations {pooled['passed_missing_structured_citations']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
