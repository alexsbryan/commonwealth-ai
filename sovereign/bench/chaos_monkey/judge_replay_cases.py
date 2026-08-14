#!/usr/bin/env python3
"""Build the pinned judge-replay case set from the recorded artifacts.

WHAT A CASE IS. One recorded judge input — (claim, evidence window, register)
— plus a label where a hand read exists. The label semantics are
SUPPORT-IN-VIEW: `not_supported_in_view` means the evidence in THIS case does
not state or clearly imply the claim, so a correctly-operating register must
flag it; `supported_in_view` means it does, so the register must clear it.
That is the only ground truth the register itself can be calibrated against —
whether a claim is *globally* true (e.g. the Indiaman provenance, real in the
corpus but absent from the judged window) is a retrieval/policy question,
out of the register's jurisdiction (see the D1 report).

INPUT SURFACES, and what each can reconstruct:

  1. `SOVEREIGN_GATE_AUDIT_FORENSICS` ledgers (results/gate_audit_forensics_*).
     BYTE-FAITHFUL for the joint register: the audit header records the full
     leaf/summary chunk texts and the per-claim cap; each claim row records
     `factual_class`, `n_shared`, and the claim-conditioned `extra` passages.
     `shared` is rebuilt exactly as `grounding/mod.rs` builds it
     (leaf[:cap], + summaries[:cap] when thematic) and `appended` exactly as
     the dedup does (first-120-chars key). VALIDATED, not assumed: this
     script recomputes n_shared for EVERY per_claim_judge row and refuses to
     emit if any mismatches (the order's not-worth-continuing gate).

  2. Harvest transcripts (results/saltgrass*_longneg_*.transcripts.jsonl and
     the land-C arm on branch wip/land-c-blocked-on-tau). NOT byte-faithful
     for the joint register: the harvest ran without forensics, so the
     claim-conditioned extras and the exact window split are unrecorded.
     Cases from this surface are marked
     `reconstruction: "transcript_shared_window_only"` — the turn's
     `retrieved_chunks` as the shared window, no appended. For the pinned
     adversarial specimens this is conservative: their fabricated specifics
     have ZERO hits in the full turn evidence, so no sub-window can make
     them supported-in-view.

USAGE (from repo root; the land-C transcripts must be extracted first when
building on main):
    git show wip/land-c-blocked-on-tau:sovereign/bench/chaos_monkey/results/saltgrass_longneg_20260813c.transcripts.jsonl > /tmp/salt_c.jsonl
    git show wip/land-c-blocked-on-tau:sovereign/bench/chaos_monkey/results/saltgrass_compound_longneg_20260813c.transcripts.jsonl > /tmp/comp_c.jsonl
    python3 sovereign/bench/chaos_monkey/judge_replay_cases.py \
        --c-arm-salt /tmp/salt_c.jsonl --c-arm-comp /tmp/comp_c.jsonl \
        --out sovereign/bench/chaos_monkey/judge_replay_cases_v1.jsonl

Deterministic: same inputs -> byte-identical output (sorted, no timestamps).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"

LEDGERS = {
    "landed14": RESULTS / "gate_audit_forensics_20260814_landed.jsonl",
    "arm2": RESULTS / "gate_audit_forensics_20260813_arm2.jsonl",
    "d0pre": RESULTS / "gate_audit_forensics_20260813_d0pre.jsonl",
    # 21 fresh turns at the landed set (commit 4cb8ee5c) — no hand labels
    # yet; feeds the --full delta sweep and the byte-faithfulness audit.
    "pbase14": RESULTS / "gate_audit_forensics_20260814_portfolio_baseline.jsonl",
}

# ---------------------------------------------------------------------------
# Hand labels for forensics-ledger claims, keyed by (ledger, claim substring).
# Sources: fabrication_etiology_20260814.md specimen table (commit 2eef5117),
# note 95b82f97 (the §7.8 hand read), note 139ab0be / d474ac24 (the land-B/C
# dropped-catch reads). A claim without a row here stays UNLABELED (emitted
# with label null for delta reporting, excluded from operating curves).
# ---------------------------------------------------------------------------
FORENSIC_LABELS: list[tuple[str, str, str, str]] = [
    # (ledger, claim-substring, label, etiology). Substrings were checked
    # against every row they catch (worker session 2026-08-13); a pattern
    # that caught rows outside its hand-read specimen was tightened rather
    # than allowed to label by adjacency (Hume-alone-conditional and the
    # generic Edwards rows stay UNLABELED for exactly that reason).
    # -- negatives: the register must flag (not supported in the judged view)
    ("landed14", "no viable form of determinism supports", "not_supported_in_view", "garble(i)"),
    ("landed14", "Derk Pereboom are identified as Hard Incompatibilists", "not_supported_in_view", "stitch(ii)"),
    ("landed14", "Hector-Miguel Mele", "not_supported_in_view", "garble(i)+parametric(iii)"),
    # spec 05 (Hume-AND-Hobbes conditional) is deliberately ABSENT: the
    # etiology reads it as a stitch(ii), but the register's own prompt
    # licenses "support assembled across several passages", and L18
    # (Hobbes+Hume as classical compatibilists) + L27 (classical
    # compatibilists analyzed ability-to-do-otherwise conditionally) is an
    # arguable assembly. A label two hand-reads disagree on must not anchor
    # a 22-case curve — the twin rows (vp .0083 leaf-only vs .9980 with
    # summaries, same pairing) stay UNLABELED and are reported as the
    # register-instability finding instead.
    ("landed14", "who bridges", "not_supported_in_view", "stitch(ii)"),  # spec 14 Kane-as-compatibilist
    # spec 16 is the foreknowledge/libertarian MASH specifically; plain
    # "Edwards was a classical compatibilist" claims are L17-supported.
    ("landed14", "classical compatibilist responses", "not_supported_in_view", "garble(i)"),
    ("landed14", "pessimist version of metaphysical libertarianism", "not_supported_in_view", "garble(i)"),
    ("landed14", "rejecting both compatibilist conditional analyses", "not_supported_in_view", "summary_carriage(iv)"),
    ("landed14", "illustrates how agents might lack alternative", "not_supported_in_view", "summary_carriage(iv)"),
    ("landed14", "No Forking Paths argument against incompatibilism", "not_supported_in_view", "summary_carriage(iv)+garble(i)"),
    ("landed14", "are modern compatibilists who argue for reasons-responsiveness", "not_supported_in_view", "summary_carriage(iv)"),
    ("arm2", "rejected conditional analysis for requiring categorical substitutability", "not_supported_in_view", "summary_carriage(iv)"),
    ("arm2", "bondage of the will", "not_supported_in_view", "garble(i)"),
    ("arm2", 'proposed the "No Forking Paths" thought experiment', "not_supported_in_view", "summary_carriage(iv)"),
    ("d0pre", "via computational irreducibility", "not_supported_in_view", "garble(i)"),
    ("d0pre", "Fischer and Paul Russell advanced", "not_supported_in_view", "summary_carriage(iv)"),
    # -- positives: the register must clear (supported in the judged view)
    ("landed14", 'coined the term "hard determinism"', "supported_in_view", "judge_false_positive(vi): verbatim at leaf[13], in-window; recorded FAILED at vp .9845"),
    ("landed14", 'coined the terms "hard determinism"', "supported_in_view", "leaf[13] verbatim"),
    ("landed14", "Berofsky (1987) and Vihvelin (2013)", "supported_in_view", "leaf-verbatim"),
    ("landed14", "Hume and Hobbes are cited as figures associated with Classical Compatibilism", "supported_in_view", "leaf-supported"),
    ("d0pre", "utilized cellular automata models to support compatibilism", "supported_in_view", "leaf[10] verbatim (etiology spec 21 citation)"),
    ("d0pre", "argue for compatibilism from the perspective of cellular automata", "supported_in_view", "leaf[10] verbatim"),
    # The DILUTION specimen (etiology D1 fold-in, seat handoff 2026-08-13):
    # leaf[18]-supported necessity-distinction claim, still FAILED at
    # n_shared=36 with the supporting chunk fully in view — while a dozen
    # near-identical wordings in the same ledger clear at vp .01-.32. The
    # residual false-positive mechanism after the window repair is the
    # register itself under a 36-chunk load, not window membership.
    ("pbase14", "general necessity imposed by nature and specific constraints", "supported_in_view", "judge_false_positive(vi)/dilution: leaf[18]-supported, failed at full membership; sibling wordings clear at .01-.32"),
]

# Scan items with hand labels (etiology specimens 09/11/15/20/21 + unnamed
# false positives). Applied to scan cases by substring match on the flagged
# item. `should_not_flag` = the item is stated in the scan's own view
# (leaf+summaries on this build); `should_flag` = a real fabrication.
SCAN_ITEM_LABELS: list[tuple[str, str, str]] = [
    ("Stephen Wolfram", "should_not_flag", "judge_false_positive(vi): verbatim leaf[10], in the scan's view"),
    ("reasons-responsiveness or Strawsonian", "should_not_flag", "summary-verbatim (S1); scan sees summaries since 95b82f97 fix"),
    ("No Forking Paths", "should_not_flag", "summary-verbatim (S0) in the scan's view; S0 itself is a corrupted summary — corpus-level issue, out of the register's jurisdiction"),
    ("hard incompatibilism", "should_not_flag", "leaf-verbatim L0/L20"),
    ("soft determinism", "should_not_flag", "leaf-verbatim L13"),
    ("hard determinist", "should_not_flag", "leaf-verbatim L0/L1/L6/L9"),
    ("Keynes", "should_flag", "parametric(iii): Keynes absent from all 36 chunks"),
    ("bridges both sides", "should_flag", "stitch(ii): Kane-as-bridge stated nowhere"),
    ("Chisholm and Pereboom", "should_flag", "stitch(ii): binding minted from roster adjacency"),
]

# Adversarial specimens from the harvest arms (the land-C (c) clearances +
# land B's (c)) — pinned as regression cases. Derived by the cross-arm
# dropped-catch join (this order) from the seat's (a)/(b)/(c) reads
# (notes d474ac24, 139ab0be). Each lists the zero-hit phrases that make it
# not-supported-in-view over the FULL turn evidence.
HARVEST_SPECIMENS = [
    {
        "case_id": "c-clear-lessa-watch",
        "arm": "salt_c",
        "probe": "longneg-distract-lessa-watch",
        "claim_substr": "severe weather rather than deliberate scuttling",
        "zero_hit": ["severe weather", "scuttl"],
        "prior_catch": "saltgrass_longneg_20260813 failed_once (byte-identical modulo emphasis)",
    },
    {
        "case_id": "c-clear-evidence-chain-indiaman",
        "arm": "salt_c",
        "probe": "longneg-distract-evidence-chain",
        "claim_substr": "wrecked Indiaman sixty years",
        "zero_hit": ["Indiaman", "sixty years"],
        "prior_catch": "saltgrass_longneg_20260808 + _20260813 failed_once",
    },
    {
        "case_id": "c-clear-officials-selfinflicted",
        "arm": "salt_c",
        "probe": "longneg-fabspec-officials",
        "claim_substr": "could not have been self-inflicted",
        "zero_hit": ["self-inflicted"],
        "prior_catch": "saltgrass_longneg_20260813 failed_once (sibling wording)",
        "note": "borderline of the four: evidence implies force/no-fall, never addresses self-infliction; kept per the seat's four-(c) read",
    },
    {
        "case_id": "c-clear-motive-coverup",
        "arm": "salt_c",
        "probe": "present-motive",
        "claim_substr": "cover up fraud involving forged salvage evidence",
        "zero_hit": ["forged salvage evidence", "cover up"],
        "prior_catch": "saltgrass_longneg_20260813b failed_once (sibling wording)",
    },
    {
        "case_id": "b-clear-salvage-pattern-hook",
        "arm": "salt_b",
        "probe": "longneg-distract-evidence-chain",
        "claim_substr": "salvage-pattern boat-hook",
        "zero_hit": ["salvage-pattern"],
        "prior_catch": "land B's one (c) (note 139ab0be correction 1); near-miss vocabulary 'salvage gear' in chunk tail manufactured the presence",
    },
]


def read_jsonl(path: Path | str):
    out = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def rebuild_window(audit: dict, claim_row: dict) -> tuple[list[str], list[str], int]:
    """Rebuild (shared, appended, expected_n_shared) exactly as
    grounding/mod.rs does at the `claim_violation_joint` call site."""
    cap = audit["per_claim_chunks"]
    leaf = audit.get("leaf_chunks") or []
    summ = audit.get("summary_chunks") or []
    shared = list(leaf[:cap])
    factual = claim_row.get("factual_class")
    if factual is False:
        shared.extend(summ[:cap])
    seen = {c[:120] for c in shared}
    appended = [c for c in (claim_row.get("extra") or []) if c[:120] not in seen]
    return shared, appended, claim_row.get("n_shared")


def forensics_cases() -> tuple[list[dict], dict]:
    cases = []
    audit_stats = {}
    for ledger, path in LEDGERS.items():
        rows = read_jsonl(path)
        audits = {r["audit_id"]: r for r in rows if r.get("kind") == "audit"}
        checked = mismatched = 0
        for r in rows:
            if r.get("kind") != "claim" or r.get("mechanism") != "per_claim_judge":
                continue
            audit = audits.get(r["audit_id"])
            if audit is None:
                continue
            shared, appended, recorded_n = rebuild_window(audit, r)
            checked += 1
            ok = recorded_n is None or len(shared) == recorded_n
            if not ok:
                mismatched += 1
                print(
                    f"  !! n_shared mismatch {ledger} {r['audit_id'][:8]} claim_idx={r.get('claim_idx')}: "
                    f"rebuilt {len(shared)} recorded {recorded_n}",
                    file=sys.stderr,
                )
            label = etiology = None
            for led, sub, lab, eti in FORENSIC_LABELS:
                if led == ledger and sub in (r.get("claim") or ""):
                    label, etiology = lab, eti
                    break
            # A summary-carriage negative is negative ONLY for a leaf-only
            # window. When this row was classed THEMATIC the summaries were
            # in its view and the same claim may be supported — support-in-
            # view is a property of (claim, window), not of the claim.
            if (
                label == "not_supported_in_view"
                and etiology
                and "summary_carriage" in etiology
                and "garble" not in etiology  # a garble is unsupported in ANY window
                and r.get("factual_class") is False
            ):
                print(
                    f"  -- {ledger} {r['audit_id'][:8]} c{r.get('claim_idx')}: summary-carriage label "
                    "withheld (thematic window included the summaries)",
                    file=sys.stderr,
                )
                label = etiology = None
            cases.append(
                {
                    "case_id": f"{ledger}-{r['audit_id'][:8]}-c{r.get('claim_idx')}",
                    "register": "per_claim_judge",
                    "label": label,
                    "etiology": etiology,
                    "claim": r["claim"],
                    "shared_chunks": shared,
                    "appended_chunks": appended,
                    "reconstruction": "forensics_exact",
                    "recorded": {
                        "artifact": path.name,
                        "audit_id": r["audit_id"],
                        "claim_idx": r.get("claim_idx"),
                        "vp": r.get("vp"),
                        "failed": r.get("failed"),
                        "tau": r.get("tau"),
                        "n_shared": recorded_n,
                        "factual_class": r.get("factual_class"),
                    },
                }
            )
        audit_stats[ledger] = {"checked": checked, "n_shared_mismatches": mismatched}
    return cases, audit_stats


def scan_cases() -> list[dict]:
    """One case per landed14 audit pass that ran the scan, with labeled items.
    Evidence = leaf + summaries, exactly what gate_longform passes on this
    build (mod.rs `scan_evidence`)."""
    rows = read_jsonl(LEDGERS["landed14"])
    audits = {r["audit_id"]: r for r in rows if r.get("kind") == "audit"}
    flagged_by_audit: dict[str, list[str]] = {}
    for r in rows:
        if r.get("kind") == "claim" and r.get("mechanism") == "specifics_scan":
            flagged_by_audit.setdefault(r["audit_id"], []).append(r["claim"])
    cases = []
    for audit_id, flagged in sorted(flagged_by_audit.items()):
        audit = audits[audit_id]
        labeled = []
        for item in flagged:
            for sub, lab, why in SCAN_ITEM_LABELS:
                if sub.lower() in item.lower():
                    labeled.append({"match": sub, "recorded_item": item, "label": lab, "why": why})
                    break
        cases.append(
            {
                "case_id": f"scan-landed14-{audit_id[:8]}",
                "register": "specifics_scan",
                "label": None,
                "labeled_items": labeled,
                "question": audit["question"],
                "answer": audit["answer"],
                # SPLIT, not pre-joined: main's scan takes one joined list
                # (leaf then summaries — the replay verb joins in that order,
                # matching gate_longform's `scan_evidence`), while land C's
                # scan signature takes the two tiers separately. Storing the
                # split keeps one case file replayable on BOTH sides of the
                # A/B the harness exists to run.
                "leaf_chunks": list(audit.get("leaf_chunks") or []),
                "summary_chunks": list(audit.get("summary_chunks") or []),
                "max_items": audit.get("budget", 10),
                "reconstruction": "forensics_exact",
                "recorded": {
                    "artifact": LEDGERS["landed14"].name,
                    "audit_id": audit_id,
                    "flagged_items": flagged,
                },
            }
        )
    return cases


def chunk_judge_cases() -> list[dict]:
    """Singular-register smoke cases pinned from the recorded evidence
    universe (the free-will iconic query: 28 leaves constant across landed14
    audits). The chunk-judge register's calibration artifact remains the
    bench critic lane (live_runner.rs); these pin replay parity, not a new
    calibration."""
    rows = read_jsonl(LEDGERS["landed14"])
    audit = next(r for r in rows if r.get("kind") == "audit")
    leaves = audit.get("leaf_chunks") or []

    def leaf_with(substr):
        for i, c in enumerate(leaves):
            if substr.lower() in c.lower():
                return i, c
        return None, None

    specs = [
        (
            "cj-luck-objection-mele",
            "Luck Objection",
            "Mele raises the Luck Objection regarding moral responsibility grounded in random events.",
            "supported_in_view",
            "note 95b82f97: leaf states 'The Luck Objection (Mele): ...' — the §7.8 hand read scored the vp=0.942 fail a plain judge error",
        ),
        (
            "cj-james-hard-determinism",
            "hard determinism and soft determinism",
            "William James coined the terms hard determinism and soft determinism.",
            "supported_in_view",
            "etiology spec 07's passage: James 'defined the common terms hard determinism and soft determinism'",
        ),
        (
            "cj-james-bondage-coinage",
            "hard determinism and soft determinism",
            'William James coined the term "bondage of the will".',
            "not_supported_in_view",
            "etiology spec 27: the phrase appears only inside James's characterization; coinage is the drafter's promotion",
        ),
    ]
    cases = []
    for cid, substr, claim, label, why in specs:
        idx, passage = leaf_with(substr)
        if passage is None:
            print(f"  -- chunk_judge case {cid}: anchor {substr!r} not in recorded leaves; SKIPPED (absence reported)", file=sys.stderr)
            continue
        cases.append(
            {
                "case_id": cid,
                "register": "chunk_judge",
                "label": label,
                "etiology": why,
                "claim": claim,
                "passage": passage,
                "reconstruction": "forensics_exact",
                "recorded": {"artifact": LEDGERS["landed14"].name, "leaf_index": idx},
            }
        )
    return cases


def batched_cases(full: bool) -> list[dict]:
    """One case per audit pass for the BATCHED register (order audit-economy
    D1): the batched pre-pass judges the LEAF window against the audit's FULL
    extracted claim list in ONE generation, exactly as `gate_longform` calls
    `claims_support_batched` (leaf[:cap], claims[:budget], before any
    exemption/veto).

    LABEL SEMANTICS ARE LEAF-VIEW. The batched view never includes summaries
    or claim-conditioned extras, so:
      * garble/stitch/parametric negatives are unsupported in ANY view — valid;
      * summary_carriage negatives are valid here EVEN for thematic rows (the
        per-claim cases withhold those labels when the thematic window included
        the summaries; the batched window never does);
      * every positive in FORENSIC_LABELS cites leaf support — checked when the
        labels were minted; a summary-only positive must NOT be added to this
        register's labels without splitting the label tables.
    `recorded.per_claim` carries the production per-claim outcome for each
    claim (mechanism/vp/failed) so the report can score batch-vs-calibrated
    agreement row by row.
    """
    cases = []
    for ledger, path in LEDGERS.items():
        rows = read_jsonl(path)
        audits = {r["audit_id"]: r for r in rows if r.get("kind") == "audit"}
        claim_rows: dict[str, list[dict]] = {}
        for r in rows:
            if r.get("kind") == "claim":
                claim_rows.setdefault(r["audit_id"], []).append(r)
        for audit_id, audit in sorted(audits.items()):
            claims = list(audit.get("claims") or [])
            if not claims:
                continue
            cap = audit["per_claim_chunks"]
            shared = list((audit.get("leaf_chunks") or [])[:cap])
            labels: list[str | None] = []
            etis: list[str | None] = []
            for c in claims:
                lab = eti = None
                for led, sub, l, e in FORENSIC_LABELS:
                    if led == ledger and sub in c:
                        lab, eti = l, e
                        break
                labels.append(lab)
                etis.append(eti)
            if not full and not any(labels):
                continue
            per_claim = [
                {k: r.get(k) for k in ("claim_idx", "claim", "mechanism", "vp", "failed", "factual_class")}
                for r in sorted(claim_rows.get(audit_id, []), key=lambda r: r.get("claim_idx") or 0)
            ]
            cases.append(
                {
                    "case_id": f"batched-{ledger}-{audit_id[:8]}",
                    "register": "batched_support",
                    "label": None,
                    "claim_labels": labels,
                    "claim_etiologies": etis,
                    "claims": claims,
                    "shared_chunks": shared,
                    "reconstruction": "forensics_exact_leaf_window",
                    "recorded": {
                        "artifact": path.name,
                        "audit_id": audit_id,
                        "per_claim": per_claim,
                    },
                }
            )
    return cases


def harvest_batched_cases(salt_c: str | None, comp_c: str | None) -> list[dict]:
    """Batched variants of the pinned harvest specimens: the specimen turn's
    FULL holding list as the claim batch (realistic batch composition), the
    turn's retrieved chunks as the window, the specimen claim labeled and the
    rest null. The (c)-class kill condition reads THESE rows: a batched
    "supported" on a labeled specimen is a lost catch."""
    paths = {
        "salt_c": salt_c,
        "comp_c": comp_c,
        "salt_b": str(RESULTS / "saltgrass_longneg_20260813b.transcripts.jsonl"),
    }
    cases = []
    for spec in HARVEST_SPECIMENS:
        path = paths.get(spec["arm"])
        if not path or not Path(path).exists():
            print(f"  -- batched harvest specimen {spec['case_id']}: arm {spec['arm']} not supplied; SKIPPED (absence reported)", file=sys.stderr)
            continue
        found = False
        for t in read_jsonl(path):
            if t.get("id") != spec["probe"]:
                continue
            chunks = t.get("retrieved_chunks") or []
            ev = "\n".join(chunks)
            holdings = (t.get("epistemic_state") or {}).get("holdings") or []
            claims = [h.get("claim") or "" for h in holdings]
            labels = []
            hit = False
            for c in claims:
                if not hit and spec["claim_substr"] in c:
                    labels.append("not_supported_in_view")
                    hit = True
                else:
                    labels.append(None)
            if not hit:
                continue
            zero = {p: ev.lower().count(p.lower()) for p in spec["zero_hit"]}
            if any(zero.values()):
                print(f"  !! batched {spec['case_id']}: zero-hit phrase now has hits {zero} — label voided, SKIPPED", file=sys.stderr)
                break
            cases.append(
                {
                    "case_id": f"batched-{spec['case_id']}",
                    "register": "batched_support",
                    "label": None,
                    "claim_labels": labels,
                    "claim_etiologies": [
                        "(c)-class clearance: engineered fabrication a prior arm caught" if l else None
                        for l in labels
                    ],
                    "must_refuse_at_operating_point": True,
                    "claims": claims,
                    "shared_chunks": chunks,
                    "reconstruction": "transcript_shared_window_only",
                    "recorded": {
                        "artifact": Path(path).name,
                        "probe": spec["probe"],
                        "prior_catch": spec["prior_catch"],
                        "zero_hit_phrases": spec["zero_hit"],
                        "evidence_chars": len(ev),
                    },
                }
            )
            found = True
            break
        if not found and path and Path(path).exists():
            print(f"  !! batched harvest specimen {spec['case_id']}: claim not found in {path}", file=sys.stderr)
    return cases


def harvest_cases(salt_c: str | None, comp_c: str | None) -> list[dict]:
    paths = {
        "salt_c": salt_c,
        "comp_c": comp_c,
        "salt_b": str(RESULTS / "saltgrass_longneg_20260813b.transcripts.jsonl"),
    }
    cases = []
    for spec in HARVEST_SPECIMENS:
        path = paths.get(spec["arm"])
        if not path or not Path(path).exists():
            print(f"  -- harvest specimen {spec['case_id']}: arm {spec['arm']} not supplied; SKIPPED (absence reported)", file=sys.stderr)
            continue
        found = False
        for t in read_jsonl(path):
            if t.get("id") != spec["probe"]:
                continue
            chunks = t.get("retrieved_chunks") or []
            ev = "\n".join(chunks)
            for h in (t.get("epistemic_state") or {}).get("holdings") or []:
                claim = h.get("claim") or ""
                if spec["claim_substr"] in claim:
                    zero = {p: ev.lower().count(p.lower()) for p in spec["zero_hit"]}
                    if any(zero.values()):
                        print(f"  !! {spec['case_id']}: zero-hit phrase now has hits {zero} — label voided, SKIPPED", file=sys.stderr)
                        continue
                    cases.append(
                        {
                            "case_id": spec["case_id"],
                            "register": "per_claim_judge",
                            "label": "not_supported_in_view",
                            "etiology": "(c)-class clearance: engineered fabrication a prior arm caught",
                            "must_refuse_at_operating_point": True,
                            "claim": claim,
                            "shared_chunks": chunks,
                            "appended_chunks": [],
                            "reconstruction": "transcript_shared_window_only",
                            "recorded": {
                                "artifact": Path(path).name,
                                "probe": spec["probe"],
                                "verification": h.get("verification"),
                                "prior_catch": spec["prior_catch"],
                                "zero_hit_phrases": spec["zero_hit"],
                                "evidence_chars": len(ev),
                            },
                            **({"note": spec["note"]} if "note" in spec else {}),
                        }
                    )
                    found = True
                    break
            if found:
                break
        if not found:
            print(f"  !! harvest specimen {spec['case_id']}: claim not found in {path}", file=sys.stderr)
    return cases


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--c-arm-salt", help="land-C saltgrass transcripts (extract from branch)")
    ap.add_argument("--c-arm-comp", help="land-C compound transcripts (extract from branch)")
    ap.add_argument("--out", required=True)
    ap.add_argument(
        "--full",
        action="store_true",
        help="also emit UNLABELED forensics cases (the whole recorded population, "
        "for delta-vs-recorded sweeps). Default emits only labeled/pinned cases: "
        "the unlabeled bulk is derivable from the committed ledgers by this same "
        "script, and committing it would duplicate them at 4x the bytes.",
    )
    args = ap.parse_args()

    fcases, audit_stats = forensics_cases()
    scases = scan_cases()
    ccases = chunk_judge_cases()
    hcases = harvest_cases(args.c_arm_salt, args.c_arm_comp)
    bcases = batched_cases(args.full) + harvest_batched_cases(args.c_arm_salt, args.c_arm_comp)
    if not args.full:
        fcases = [c for c in fcases if c.get("label")]
        scases = [c for c in scases if c.get("labeled_items")]

    print("== byte-faithfulness audit (n_shared rebuilt vs recorded, per_claim_judge rows) ==")
    total_mismatch = 0
    for ledger, st in audit_stats.items():
        print(f"  {ledger}: checked {st['checked']}, mismatches {st['n_shared_mismatches']}")
        total_mismatch += st["n_shared_mismatches"]
    if total_mismatch:
        print("REFUSING to emit: recorded inputs do not rebuild the recorded window "
              "(prompt assembly changed under the recordings?)", file=sys.stderr)
        return 1

    cases = fcases + scases + ccases + hcases + bcases
    labeled = [c for c in cases if c.get("label")]
    neg = sum(1 for c in labeled if c["label"] == "not_supported_in_view")
    pos = sum(1 for c in labeled if c["label"] == "supported_in_view")
    bl = [l for c in bcases for l in (c.get("claim_labels") or []) if l]
    print(f"== cases: {len(cases)} total | per_claim_judge {sum(1 for c in cases if c['register']=='per_claim_judge')} "
          f"| chunk_judge {sum(1 for c in cases if c['register']=='chunk_judge')} "
          f"| specifics_scan {sum(1 for c in cases if c['register']=='specifics_scan')} "
          f"| batched_support {len(bcases)}")
    print(f"== labels: {neg} not_supported_in_view / {pos} supported_in_view / "
          f"{len(cases)-len(labeled)-len(bcases)} unlabeled (delta-reporting only)")
    print(f"== batched claim-level labels: {sum(1 for l in bl if l=='not_supported_in_view')} neg / "
          f"{sum(1 for l in bl if l=='supported_in_view')} pos across {len(bcases)} batch cases")

    with open(args.out, "w", encoding="utf-8") as fh:
        for c in cases:
            fh.write(json.dumps(c, ensure_ascii=False, sort_keys=True) + "\n")
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
