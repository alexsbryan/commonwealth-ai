#!/usr/bin/env python3
"""Mine four months of case law into comaintainer judgment episodes.

Spec: `docs/COMAINTAINER.md` §6; plan `enchanted-yawning-gem`. Mirrors
`gym/next-edit/golden/harvest_golden.py`: deterministic (no RNG,
every-k-th sampling), per-source and per-class quotas, dedupe by
signature, every drop counted in a printed EXCLUDED counter.

Six sources, all in v0 (operator direction 2026-08-06):

  1. ledger      — DEFAULTS_LEDGER rows: settled verdicts with rationale
  2. commit      — verdict-bearing commit messages (reject/approve)
  3. attempt     — failed-approach notes; decision notes with receipts
  4. tripwire    — invariants negated in prose, paired with clean twins
  5. transcript  — operator interventions and go-aheads, mined locally
  6. fixchain    — fix-commit F blamed back to introducing commit I:
                   review I's diff, answer key in F (tier C, never gates)

Plus evidence-manipulated twins (-t1 stripped -> measure-first,
-t2 artifact-elided -> could-not-judge) from tier-A separable parents.

Labels: episode `expect` blocks are the HOUSE's recorded verdicts, not a
model's opinion. Tier A = settled by a later instrument; B = operator-
settled; C = inferred (fix-chains, receipt-quiet commits). Gates score
tier A only.

    python3 gym/comaintainer/harvest_episodes.py
    python3 gym/comaintainer/harvest_episodes.py --no-transcripts   # CI-safe
"""

from __future__ import annotations

import argparse
import collections
import json
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import markers as M  # noqa: E402

REPO = HERE.parent.parent
LEDGER = REPO / "sovereign" / "DEFAULTS_LEDGER.md"
ARCH = REPO / "sovereign" / "ARCH_PRINCIPLES.md"
NOTES_DB = Path.home() / ".sovereign" / "notes.db"
TRANSCRIPTS = Path.home() / ".claude" / "projects" / "-Users-alexsbryan-dev-commonwealth-ai"

EXCLUDED: collections.Counter = collections.Counter()
COUNTS: collections.Counter = collections.Counter()

FIXCHAIN_WINDOW_DAYS = 45
DIFF_CAP = 8 * 1024
NOTE_SNAPSHOT_CHARS = 200


def git(*args: str) -> str:
    return subprocess.run(["git", "-C", str(REPO), *args],
                          capture_output=True).stdout.decode(errors="replace")


# ---- text surgery -----------------------------------------------------


def sentences(text: str) -> list[str]:
    return [s.strip() for s in
            re.split(r"(?<=[.!?])\s+(?=[A-Z0-9`*\[])", text) if s.strip()]


def paragraphs(text: str) -> list[str]:
    return [p.strip() for p in re.split(r"\n\s*\n", text) if p.strip()]


def is_verdict_sentence(s: str) -> bool:
    return bool(M.REJECT_RE.search(s) or M.APPROVE_RE.search(s))


def classify_paragraph(p: str) -> str:
    """VERDICT / EVIDENCE / SITUATION — the one classifier every prose
    source shares (§10.6)."""
    if any(is_verdict_sentence(s) for s in sentences(p)):
        return "VERDICT"
    if len(M.MEASURE_RE.findall(p)) >= 2 or any(
            l.count("|") >= 2 for l in p.splitlines()):
        return "EVIDENCE"
    return "SITUATION"


def strip_verdict_sentences(text: str) -> tuple[str, list[str]]:
    """Remove verdict-bearing sentences; return (clean, removed)."""
    keep, removed = [], []
    for s in sentences(text):
        (removed if is_verdict_sentence(s) else keep).append(s)
    return " ".join(keep), removed


def clean_subject(subject: str) -> str:
    """Drop dash-segments that carry the verdict ('— measured, rejected').
    If every segment carries one, MASK the verdict phrases rather than
    falling back to the raw title — the fallback would print the answer
    key on the exam."""
    parts = re.split(r"\s+[—–-]{1,2}\s+", subject)
    kept = [p for p in parts if not is_verdict_sentence(p)]
    if kept:
        return " — ".join(kept)
    masked = M.REJECT_RE.sub("[…]", parts[0])
    return M.APPROVE_RE.sub("[…]", masked)


def trunc(text: str, cap: int) -> str:
    text = text.strip()
    return text if len(text) <= cap else text[: cap - 12].rstrip() + "\n[truncated]"


def strip_states(text: str) -> str:
    return M.LEDGER_STATE_RE.sub("", text)


def episode(source: str, tier: str, anchor: str, situation: str, proposal: str,
            evidence: str, verdict: str, arg_value, rationale: str,
            basis: list[str], provenance: dict) -> dict:
    """Assemble one schema-shaped episode. `request` is the ONLY block a
    candidate ever sees."""
    COUNTS[source] += 1
    eid = f"cm-{source}-{COUNTS[source]:05d}-{anchor}"
    ep = {
        "id": eid, "source": source, "tier": tier, "split": "dev",  # stamped later
        "request": {
            "situation": trunc(strip_states(situation), M.LEN_BOUNDS["situation"]),
            "proposal": trunc(strip_states(proposal), M.LEN_BOUNDS["proposal"]),
            "evidence": trunc(strip_states(evidence), M.LEN_BOUNDS["evidence"])
                        or "[none provided]",
        },
        "expect": {
            "verdict": verdict,
            M.ARG_OF[verdict]: arg_value,
            "basis": basis,
            "rationale": rationale.strip(),
        },
        "provenance": {"anchor": anchor, **provenance},
    }
    return ep


# ---- source 1: DEFAULTS_LEDGER ---------------------------------------

EVIDENCE_KEYS = {"proof so far", "earned by", "measured", "cost of on",
                 "what settled it", "verdict"}
# "What shipped instead" is deliberately ABSENT: an audit pass found it
# semantically leaks the house's call ("don't add the slot, add the
# diversifier") into the request even though no regex fires on it.
SITUATION_KEYS = {"what it does", "what it did", "what is dark", "shipped",
                  "why it under-delivers, and this is the actionable part",
                  "honest scope note", "model scope",
                  "why they are two flags, not one", "open caveat"}


def strip_note_boilerplate(content: str) -> str:
    """Remove migration headers ('**Applies to:** …', '_Migrated from …',
    '## Index overflow…') that ride ahead of a note's real first
    sentence — an audit pass found them quoted into tripwire proposals
    as if they were the constraint."""
    out = []
    for line in content.splitlines():
        s = line.strip()
        if s.startswith("**Applies to:**") or s.startswith("_Migrated from") \
                or s.startswith("## Index overflow"):
            continue
        out.append(line)
    return "\n".join(out).strip()


def parse_ledger() -> list[dict]:
    text = LEDGER.read_text()
    eps: list[dict] = []
    section = None
    rows: list[tuple[str, str, list[str]]] = []  # (section, heading, lines)
    for line in text.splitlines():
        if line.startswith("## "):
            section = line[3:].split("—")[0].strip()
            continue
        if line.startswith("### "):
            rows.append((section, line[4:].strip(), []))
            continue
        if rows:
            rows[-1][2].append(line)

    owed = parse_owed_table(text)

    for section, heading, lines in rows:
        body = "\n".join(lines)
        slug = M.slugify(heading)
        # Heading override wins over section (multiquote row lives under
        # DARK with a GRADUATED arrow in its own title).
        state = section or ""
        if re.search(r"→\s*\*\*GRADUATED", heading):
            state = "GRADUATED"
        elif re.search(r"(?i)SETTLED.*REJECTED|REJECTED ANYWAY", body) or \
                re.search(r"(?i)\*\*Review by:\*\* closed", body):
            EXCLUDED["ledger_row_settled_in_place"] += 1
            continue  # its verdict lives in the REJECTED section row

        bullets = re.findall(r"^- \*\*([^:*]+?)[:.]?\*\*\s*(.*(?:\n(?!- \*\*| *###| *##).*)*)",
                             body, re.M)
        fields = {k.strip().lower(): re.sub(r"\s+", " ", v).strip() for k, v in bullets}
        name = strip_states(re.sub(r"[`]", "", heading)).split("—")[0].strip()

        situation_bits, evidence_bits, rationale_bits = [], [], []
        for k, v in fields.items():
            clean, removed = strip_verdict_sentences(v)
            rationale_bits += removed
            base = next((ek for ek in EVIDENCE_KEYS if k.startswith(ek)), None)
            if base:
                evidence_bits.append(clean)
            elif any(k.startswith(sk) for sk in SITUATION_KEYS):
                situation_bits.append(clean)
        situation = f"Capability: {name}. " + " ".join(situation_bits)
        prov = {"ledger_section": state, "house_verdict": state,
                "note_id": None, "commit": None, "files": []}

        if state.startswith("DARK"):
            instrument = fields.get("settled by", "").split(".")[0] or \
                "the settling instrument named in the ledger row"
            eps.append(episode(
                "ledger", "A", slug, situation,
                f"Flip the default: enable {name} for every user now, on the "
                f"strength of the proof so far.",
                " ".join(evidence_bits), "measure-first",
                instrument,
                "The flip condition is stated and unmet; partial proof does "
                "not flip a default (§18.4/§18.5).",
                [f"ledger:{slug}"], prov))
        elif state.startswith("REJECTED"):
            ask = fields.get("re-open only if", "")
            ask = ("Re-open only if: " + ask) if ask else \
                "Do not re-litigate without new evidence of the recorded kind."
            eps.append(episode(
                "ledger", "A", slug, situation,
                f"Adopt {name} as a default: enable it for every user.",
                " ".join(evidence_bits), "revise", ask,
                "The house measured this and said no; the rejection stands "
                "until its re-open condition is met.",
                [f"ledger:{slug}"], prov))
        elif state.startswith("GRADUATED"):
            eps.append(episode(
                "ledger", "A", slug, situation,
                f"Keep {name} at its current default for every user.",
                " ".join(evidence_bits), "approve",
                [f"ledger:{slug}"],
                "The flip condition was measured and met; the default is earned.",
                [f"ledger:{slug}"], prov))
        elif state.startswith("INTENTIONAL"):
            eps.append(episode(
                "ledger", "B", slug, situation,
                f"Flip {name} on by default — off looks like an oversight.",
                " ".join(evidence_bits), "revise",
                "Off is the designed end state, not a debt; do not flip it.",
                "The ledger records off as deliberate; the row exists so "
                "nobody 'fixes' the default.",
                [f"ledger:{slug}"], prov))

    eps += owed
    return eps


def parse_owed_table(text: str) -> list[dict]:
    """OWED-A-ROW table rows -> measure-first tier B."""
    m = re.search(r"## OWED A ROW.*?\n((?:\|.*\n)+)", text, re.S)
    eps = []
    if not m:
        return eps
    for row in m.group(1).splitlines():
        cells = [c.strip() for c in row.strip("|").split("|")]
        if len(cells) != 3 or cells[0].startswith("---") or cells[0] == "flag":
            continue
        flag = cells[0].strip("`")
        slug = re.sub(r"[^a-z0-9]+", "-", flag.lower()).strip("-")[:40]
        eps.append(episode(
            "ledger", "B", slug,
            f"A dark capability with no ledger row: `{flag}` — {cells[1]}. "
            f"It predates the ledger contract and has never been measured.",
            f"Graduate `{flag}` to a full ledger row with a flip condition, "
            f"citing its design intent as the proof.",
            "[none provided]", "measure-first",
            cells[2],
            "A row whose proof was invented reads as settled; the capability "
            "needs its A/B before it can carry a flip condition (§18.4).",
            ["ledger:owed-a-row"],
            {"ledger_section": "OWED", "house_verdict": "OWED",
             "note_id": None, "commit": None, "files": []}))
    return eps


# ---- source 2: verdict commits ---------------------------------------

COMMIT_GREP = (
    "measured and rejected|stays off|stays unset|net-negative|dominated"
    "|did not separate|nothing separates|no speedup|earned the default"
    "|flip condition met|moved DARK|overturn|withdraw"
)


def note_refs(text: str) -> list[str]:
    return [f"note {h}" for h in
            dict.fromkeys(re.findall(r"(?i)\bnote[s]?\s+`?([0-9a-f]{8})\b", text))]


def arch_refs(text: str) -> list[str]:
    return [f"ARCH §{s}" for s in
            dict.fromkeys(re.findall(r"§\s?(\d+(?:\.\d+)?)", text))]


def mine_commits() -> list[dict]:
    log = git("log", "--no-merges", "-E", "-i", f"--grep={COMMIT_GREP}",
              "--format=%H%x00%ad%x00%s%x00%b%x01", "--date=short")
    eps = []
    for rec in log.split("\x01"):
        rec = rec.strip("\n")
        if not rec.strip():
            continue
        commit, date, subject, body = (rec.split("\x00") + ["", "", ""])[:4]
        commit = commit.strip()
        if not body.strip():
            EXCLUDED["commit_no_body"] += 1
            continue

        paras = paragraphs(body)
        verdict_paras = [p for p in paras if classify_paragraph(p) == "VERDICT"]
        evidence_paras = [p for p in paras if classify_paragraph(p) == "EVIDENCE"]
        situation_paras = [p for p in paras if classify_paragraph(p) == "SITUATION"]

        subj_reject = bool(M.REJECT_RE.search(subject))
        subj_approve = bool(M.APPROVE_RE.search(subject))
        text_all = subject + "\n" + body
        has_reject = subj_reject or any(M.REJECT_RE.search(p) for p in verdict_paras)
        has_approve = subj_approve or any(M.APPROVE_RE.search(p) for p in verdict_paras)
        if not (has_reject or has_approve):
            EXCLUDED["commit_no_verdict_sentence"] += 1
            continue
        if has_reject and has_approve:
            EXCLUDED["commit_two_verdicts_inseparable"] += 1
            continue
        # A feat/test/docs commit that SHIPS one thing while REJECTING a
        # sibling ("ship dedup; reject the reranker") is two verdicts in
        # one landing even when only the reject family matches — the
        # landing itself was good. Audit-found mislabel class.
        if subj_reject and re.match(r"(?:feat|docs|test|chore)\b", subject) \
                and ";" in subject:
            EXCLUDED["commit_ship_plus_reject_inseparable"] += 1
            continue

        # Verdict sentences inside evidence/situation move expect-side.
        rationale_bits = []
        def scrub(ps):
            out = []
            for p in ps:
                clean, removed = strip_verdict_sentences(p)
                rationale_bits.extend(removed)
                if clean:
                    out.append(clean)
            return out
        evidence = "\n\n".join(scrub(evidence_paras))
        situation = "\n\n".join(scrub(situation_paras))
        for p in verdict_paras:
            clean, removed = strip_verdict_sentences(p)
            rationale_bits.extend(removed)
            if classify_paragraph(clean or "") == "EVIDENCE":
                evidence = (evidence + "\n\n" + clean).strip()

        subj = clean_subject(subject)
        rmatch = re.match(r'Revert "(.*)"', subject)
        if rmatch:
            proposal = f"Land the change: {rmatch.group(1)}"
            situation = (f"A prior commit landed with that title. " + situation).strip()
        else:
            proposal = f"Land the change: {subj}."
        if not situation:
            situation = "A worker reports this change ready to land on main."

        tier = "A" if len(M.MEASURE_RE.findall(evidence)) >= 2 else "B"
        basis = note_refs(text_all) + arch_refs(text_all) or [f"commit {commit[:7]}"]
        rationale = " ".join(rationale_bits)[:900] or subject
        if has_reject:
            ask_src = next((s for s in sentences(body) if M.REJECT_RE.search(s)),
                           subject)
            eps.append(episode(
                "commit", tier, commit[:7], situation, proposal, evidence,
                "revise", trunc(ask_src, 400), rationale, basis,
                {"commit": commit, "note_id": None, "ledger_section": None,
                 "house_verdict": "reject", "date": date, "files": []}))
        else:
            eps.append(episode(
                "commit", tier, commit[:7], situation, proposal, evidence,
                "approve", basis, rationale, basis,
                {"commit": commit, "note_id": None, "ledger_section": None,
                 "house_verdict": "approve", "date": date, "files": []}))
    return eps


def mine_benign_commits(cap: int = 20) -> list[dict]:
    """Receipt-bearing commits ≥30 days quiet -> approve, tier C."""
    log = git("log", "--no-merges", "--before=30 days ago", "--since=2026-03-31",
              "--format=%H%x00%ad%x00%s%x00%b%x01", "--date=short")
    eps = []
    for rec in log.split("\x01"):
        if len(eps) >= cap:
            break
        rec = rec.strip("\n")
        if not rec.strip():
            continue
        commit, date, subject, body = (rec.split("\x00") + ["", "", ""])[:4]
        commit = commit.strip()
        if not body.strip() or not M.RECEIPT_RE.search(body):
            continue
        if M.REJECT_RE.search(subject + body) or M.APPROVE_RE.search(subject + body):
            continue  # verdict commits are source 2's, not benign
        files = git("diff-tree", "-r", "--no-renames", "--name-only",
                    commit).splitlines()[1:]
        nondocs = [f for f in files if not f.endswith(".md")]
        if not nondocs or len(files) > 25:
            EXCLUDED["benign_shape"] += 1
            continue
        receipts = [s for s in sentences(body) if M.RECEIPT_RE.search(s)]
        rest, _ = strip_verdict_sentences(
            " ".join(s for s in sentences(body) if s not in receipts))
        eps.append(episode(
            "commit", "C", commit[:7],
            f"A worker reports this change ready to land ({len(files)} files: "
            + ", ".join(files[:6]) + ("…" if len(files) > 6 else "") + "). " + rest,
            f"Land the change: {clean_subject(subject)}.",
            "Gate receipts from the worker: " + " ".join(receipts),
            "approve", [f"commit {commit[:7]}"],
            "Receipts show the gates ran; scope is bounded; nothing in the "
            "landing contradicts a recorded decision.",
            [f"commit {commit[:7]}"],
            {"commit": commit, "note_id": None, "ledger_section": None,
             "house_verdict": "quiet-30d", "date": date, "files": files[:25]}))
    return eps


# ---- source 3: notes (attempts + decisions) ---------------------------


def notes_conn():
    return sqlite3.connect(f"file:{NOTES_DB}?mode=ro", uri=True)


def note_snapshot(content: str, files, symbols, chash) -> dict:
    return {"files": files, "symbols": symbols, "content_hash": chash,
            "head200": content[:NOTE_SNAPSHOT_CHARS]}


def mine_attempts() -> list[dict]:
    eps = []
    with notes_conn() as db:
        rows = db.execute(
            "SELECT id, content, files, symbols, content_hash FROM notes "
            "WHERE kind='attempt' AND tombstone=0 AND retired_at IS NULL "
            "ORDER BY rowid").fetchall()
    for nid, content, files, symbols, chash in rows:
        files_l = json.loads(files or "[]")
        symbols_l = json.loads(symbols or "[]")
        content = strip_note_boilerplate(content)
        # Numbered multi-attempt notes split into one episode per item.
        items = re.split(r"\n\s*(?=\d+\.\s+\*\*)", content)
        if len(items) == 1:
            items = [content]
        for idx, item in enumerate(items):
            item = item.strip()
            if len(item) < 80:
                EXCLUDED["attempt_too_short"] += 1
                continue
            sents = sentences(item)
            result = [s for s in sents if M.ATTEMPT_RESULT_RE.search(s)
                      or is_verdict_sentence(s)]
            setup = [s for s in sents if s not in result]
            if not result or not setup:
                EXCLUDED["attempt_unsplittable"] += 1
                continue
            lesson = next((s for s in sents if re.search(
                r"(?i)\blesson\b|\bbench\b|measure|A/B", s)), None)
            proposal = " ".join(setup[:4])
            situation = ("A worker proposes the following approach for a "
                         "problem in " + (", ".join(files_l[:3]) or
                                          ", ".join(symbols_l[:3]) or "this area") + ".")
            prov = {"note_id": nid, "commit": None, "ledger_section": None,
                    "house_verdict": "attempt-failed", "files": files_l,
                    "note_snapshot": note_snapshot(content, files_l, symbols_l, chash)}
            anchor = nid[:8] + (f"i{idx}" if len(items) > 1 else "")
            if lesson and M.INSTRUMENT_RE.search(lesson):
                eps.append(episode(
                    "attempt", "A", anchor, situation, proposal, "[none provided]",
                    "measure-first", M.INSTRUMENT_RE.search(lesson).group(0),
                    trunc(" ".join(result[:3]), 600),
                    [f"note {nid[:8]}"], prov))
            else:
                eps.append(episode(
                    "attempt", "A", anchor, situation, proposal, "[none provided]",
                    "revise",
                    "Do not take this path; it was tried and failed. "
                    "Consult the cited attempt note before proposing a variant.",
                    trunc(" ".join(result[:3]), 600),
                    [f"note {nid[:8]}"], prov))
    return eps


def mine_decisions(cap: int) -> list[dict]:
    eps = []
    with notes_conn() as db:
        echo = db.execute(
            "SELECT COUNT(*) FROM notes WHERE kind='decision' AND "
            "source='committed'").fetchone()[0]
        rows = db.execute(
            "SELECT id, content, files, symbols, content_hash FROM notes "
            "WHERE kind='decision' AND source='agent' AND tombstone=0 AND "
            "retired_at IS NULL ORDER BY rowid").fetchall()
    print(f"  note_decision_commit_echo={echo} (source='committed', excluded "
          f"as a class)", file=sys.stderr)
    candidates = []
    for nid, content, files, symbols, chash in rows:
        content = strip_note_boilerplate(content)
        if len(M.MEASURE_RE.findall(content)) < 2:
            EXCLUDED["decision_unmeasured"] += 1
            continue
        sents = sentences(content)
        vs = [s for s in sents if is_verdict_sentence(s)]
        if not vs:
            EXCLUDED["decision_no_verdict_sentence"] += 1
            continue
        # A note carrying BOTH families is usually a delivered feature
        # with an embedded sub-rejection ("VALIDATED end-to-end … gossip
        # — rejected"); labeling the whole note by either family is
        # wrong. Audit-found mislabel class; same rule as commits.
        if any(M.REJECT_RE.search(s) for s in vs) and \
                any(M.APPROVE_RE.search(s) for s in vs):
            EXCLUDED["decision_two_verdicts_inseparable"] += 1
            continue
        candidates.append((nid, content, json.loads(files or "[]"),
                           json.loads(symbols or "[]"), chash, vs))
    k = max(1, len(candidates) // cap + (1 if len(candidates) % cap else 0))
    picked = candidates[::k][:cap]
    EXCLUDED["decision_every_kth_skipped"] += len(candidates) - len(picked)
    for nid, content, files_l, symbols_l, chash, vs in picked:
        paras = paragraphs(content.replace(" ¶ ", "\n\n"))
        rationale_bits = []
        def scrub(kind):
            out = []
            for p in paras:
                if classify_paragraph(p) != kind:
                    continue
                clean, removed = strip_verdict_sentences(p)
                rationale_bits.extend(removed)
                if clean:
                    out.append(clean)
            return out
        evidence = "\n\n".join(scrub("EVIDENCE"))
        situation = "\n\n".join(scrub("SITUATION")) or \
            "A worker records this decision as settled and ready to act on."
        for p in paras:
            if classify_paragraph(p) == "VERDICT":
                clean, removed = strip_verdict_sentences(p)
                rationale_bits.extend(removed)
                if clean and classify_paragraph(clean) == "EVIDENCE":
                    evidence = (evidence + "\n\n" + clean).strip()
        reject = any(M.REJECT_RE.search(s) for s in vs)
        tier = "A"
        prov = {"note_id": nid, "commit": None, "ledger_section": None,
                "house_verdict": "reject" if reject else "approve",
                "files": files_l,
                "note_snapshot": note_snapshot(content, files_l, symbols_l, chash)}
        first_clause = sentences(content)[0][:160]
        if reject:
            eps.append(episode(
                "decision", tier, nid[:8], situation,
                f"Adopt and build on: {first_clause}",
                evidence, "revise",
                trunc(next(s for s in vs if M.REJECT_RE.search(s)), 400),
                trunc(" ".join(rationale_bits), 700),
                [f"note {nid[:8]}"], prov))
        else:
            eps.append(episode(
                "decision", tier, nid[:8], situation,
                f"Adopt and build on: {first_clause}",
                evidence, "approve", [f"note {nid[:8]}"],
                trunc(" ".join(rationale_bits), 700),
                [f"note {nid[:8]}"], prov))
    return eps


# ---- source 4: tripwires from invariants + constructed ---------------


def mine_tripwires(cap_pairs: int = 50) -> list[dict]:
    eps = []
    with notes_conn() as db:
        rows = db.execute(
            "SELECT id, content, files, symbols, content_hash FROM notes "
            "WHERE kind='invariant' AND tombstone=0 AND retired_at IS NULL "
            "ORDER BY rowid").fetchall()
    usable = []
    for nid, content, files, symbols, chash in rows:
        files_l = json.loads(files or "[]")
        symbols_l = json.loads(symbols or "[]")
        content = strip_note_boilerplate(content)
        first = sentences(content)[0] if sentences(content) else ""
        if not (files_l or symbols_l) or not (40 < len(first) < 400):
            EXCLUDED["invariant_unanchored_or_odd_length"] += 1
            continue
        if not re.search(r"(?i)\b(never|must|cannot|can'?t|don'?t|do not|only"
                         r"|always|requires?|forbid|off unless|not)\b", first):
            EXCLUDED["invariant_not_prohibition_shaped"] += 1
            continue
        usable.append((nid, content, files_l, symbols_l, chash, first))
    k = max(1, len(usable) // cap_pairs + (1 if len(usable) % cap_pairs else 0))
    picked = usable[::k][:cap_pairs]
    EXCLUDED["invariant_every_kth_skipped"] += len(usable) - len(picked)
    for nid, content, files_l, symbols_l, chash, first in picked:
        where = ", ".join((files_l or symbols_l)[:3])
        prov = {"note_id": nid, "commit": None, "ledger_section": None,
                "house_verdict": "invariant", "files": files_l,
                "note_snapshot": note_snapshot(content, files_l, symbols_l, chash)}
        # Planted: the proposal negates the recorded constraint. The
        # expect side deliberately does NOT quote the constraint back
        # (leak_rationale_shingle would trip); the basis note carries it.
        eps.append(episode(
            "tripwire", "A", nid[:8] + "p0",
            f"A worker's diff touches {where}. The change is described as a "
            f"cleanup with no functional intent.",
            f"Simplify {where}: remove the guard/special-case enforcing "
            f"“{first}” — it looks redundant and costs a branch.",
            "[none provided]", "revise",
            "Restore the constraint; it is a recorded invariant, not dead "
            "code. Cite the note before touching that path again.",
            "The proposal negates a recorded invariant; cleanups do not get "
            "to delete constraints they cannot see the reason for.",
            [f"note {nid[:8]}"], {**prov, "pair_of": f"{nid[:8]}p1"}))
        eps.append(episode(
            "tripwire", "A", nid[:8] + "p1",
            f"A worker's diff touches {where}. The change is described as a "
            f"cleanup with no functional intent.",
            f"In {where}: add a test that fails when the recorded constraint "
            f"there is violated, and a comment naming why it exists. No "
            f"behavior change.",
            "Worker receipts: the new test fails when the guard is removed "
            "and passes on the current code; full suite green.",
            "approve", [f"note {nid[:8]}"],
            "Structural enforcement of a recorded invariant, with the gate "
            "watched to fail — exactly the encoding the house asks for.",
            [f"note {nid[:8]}"], {**prov, "pair_of": f"{nid[:8]}p0"}))
    return eps


def parse_smell_table() -> list[tuple[str, str]]:
    text = ARCH.read_text()
    m = re.search(r"##+ *§?15[^\n]*\n(.*?)(?=\n##+ *§?1[67])", text, re.S)
    if not m:
        return []
    rows = []
    for line in m.group(1).splitlines():
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) == 2 and cells[0] and not cells[0].startswith("-") \
                and cells[0].lower() != "smell":
            ref = re.sub(r"[^\d.]", "", cells[1])
            if ref:
                rows.append((cells[0], ref))
    return rows


CONSTRUCTED = [
    # (key, situation, proposal, evidence, verdict, arg, rationale, basis)
    ("esc-priority",
     "Two approved orders are in flight: the retrieval-latency initiative and "
     "the desktop-updater hardening. A worker frees up.",
     "Assign the freed worker to retrieval-latency — it looks higher impact.",
     "[none provided]", "escalate",
     "Which initiative takes the freed worker: retrieval-latency or "
     "desktop-updater hardening?",
     "Relative product priority between two approved initiatives is the "
     "operator's call, permanently.", ["ARCH §14"]),
    ("esc-budget",
     "The paired engine slice shows material disagreement between the local "
     "judge and the frontier judge on the holdout.",
     "Spend the reserved frontier-call budget on the full confirmation run.",
     "Paired slice: engine agreement below the recorded bar; both deltas "
     "recorded in the run meta.", "escalate",
     "The confirmation run consumes the session's entire frontier budget — "
     "spend it now, or accept the local verdict?",
     "Budget is operator-owned; the role recommends, the operator spends.",
     ["ARCH §18.4"]),
    ("esc-privacy",
     "Transcript mining surfaced strong training episodes that quote operator "
     "messages verbatim, including project names not in the public repo.",
     "Commit the episodes as mined — the value is in the verbatim text.",
     "[none provided]", "escalate",
     "Which classes of transcript-derived text may be committed to the repo: "
     "verbatim, paraphrase-only, or pointer-only?",
     "Privacy boundaries on operator-authored text are operator-owned.",
     ["ARCH §7"]),
    ("esc-taste",
     "Two working names exist for the new role and both appear in docs.",
     "Standardize on one name across CLI verbs, ledger rows and directory "
     "names before more surface accretes.",
     "[none provided]", "escalate",
     "Which name ossifies into the CLI/ledger/directory surface?",
     "Naming that users will type is taste; taste stays with the operator.",
     ["ARCH §14"]),
    ("cnj-no-artifact",
     "A worker reports a 1.4x latency win on the synthesis path.",
     "Land the optimization; the win is stated in the report.",
     "The full run log is on the worker's machine and was not attached; no "
     "run directory, no numbers, no baseline reference in the report.",
     "could-not-judge",
     "the run artifact (log or runs/<stamp> directory) with baseline and "
     "delta; without it there is nothing to verify",
     "A stated multiplier with no inspectable artifact cannot be judged; "
     "honoring could-not-judge beats fabricating confidence (§18.2).",
     ["ARCH §18.2"]),
    ("cnj-gate-never-ran",
     "A landing claims the full workspace suite passed.",
     "Approve the landing on the strength of the green suite.",
     "The attached test summary shows `pass: 0 fail: 0` with an empty "
     "resolved scope banner.",
     "could-not-judge",
     "a test run that actually executed tests — a zero-test run verified "
     "nothing and its exit code is not evidence",
     "pass: 0 fail: 0 is a could-not-judge, not a pass; the gate never ran "
     "(§18.2).", ["ARCH §18.2"]),
    ("cnj-instrument-unvalidated",
     "A new bench lane was written this session and immediately used to "
     "justify a default flip.",
     "Flip the default; the new lane shows a 12-point improvement.",
     "One run of the brand-new lane, no noise floor, no negative control, "
     "no baseline predating the change.",
     "could-not-judge",
     "the lane's own validation: a noise pair and a negative control run "
     "before any delta from it is readable",
     "An uninstrumented instrument cannot certify a result (§18.4); the "
     "12 points are unreadable until the lane is validated.",
     ["ARCH §18.4"]),
    ("esc-review-by",
     "A worker's change touches the retrieval prefilter, whose ledger row is "
     "past its review-by date.",
     "Re-date the row by a month so the landing is unblocked; nobody has "
     "bandwidth for the settling measurement right now.",
     "[none provided]", "escalate",
     "The row is past review-by: flip it, kill it, or re-date it with a "
     "named blocker — which?",
     "'Still waiting' without a named blocker is not a valid ledger state, "
     "and the three-way call is the operator's by construction.",
     ["ledger:corpus-relevance-prefilter"]),
    ("esc-user-default",
     "A measured win exists for a new answer-length default, but it changes "
     "what every existing user sees on every turn.",
     "Flip the default in this landing; the numbers support it.",
     "A/B on the house bank: quality metric up, latency flat, n=180, CIs "
     "non-overlapping.", "escalate",
     "The measurement supports the flip; changing every user's observed "
     "behavior is a product call — flip now or stage it?",
     "Product-visible behavior changes are operator-owned even when the "
     "measurement is clean.", ["ARCH §18"]),
    ("esc-data-purge",
     "A migration would drop a legacy table that still holds user "
     "conversation rows on some installs.",
     "Ship the migration with the drop; the table is legacy and the code "
     "no longer reads it.",
     "[none provided]", "escalate",
     "The drop destroys user data on installs that still carry rows — "
     "proceed, gate on a backup, or keep the table?",
     "Destroying user data is never the role's call; privacy and "
     "irreversibility both route to the operator.", ["ARCH §7"]),
    ("esc-release-timing",
     "A feature is gate-green and demo-ready two days before a planned "
     "release cut.",
     "Land it on main today so it rides the release.",
     "Lint, test and the named bench lane all green; receipts attached.",
     "escalate",
     "Ride this release or hold for the next — where does the risk "
     "tolerance sit this close to the cut?",
     "Release timing is product priority, not landing quality; green gates "
     "do not answer it.", ["ARCH §14"]),
    ("esc-frontier-spend",
     "The local judge and the frontier judge disagree on 9 of 60 paired "
     "verdicts; the budget cap for frontier calls is nearly consumed.",
     "Adopt the local judge's verdicts for the disagreeing nine and move on.",
     "Paired slice recorded: agreement 51/60; per-episode disagreement list "
     "attached.", "escalate",
     "Spend the remaining frontier budget adjudicating the nine, or accept "
     "the local verdicts and record the disagreement?",
     "Spending the reserved budget is the operator's call; the role's job "
     "is to present the disagreement, not to absorb it.", ["ARCH §18.4"]),
    ("esc-public-name",
     "A new CLI verb is about to ship and its name will appear in docs, "
     "completions and user muscle memory.",
     "Ship under the working name; renaming later is a deprecated-alias "
     "away.",
     "[none provided]", "escalate",
     "Which name ships? Renames after adoption cost a deprecation cycle "
     "and user retraining.",
     "Names users type are taste plus a compatibility commitment; both "
     "belong to the operator.", ["ARCH §2"]),
    ("esc-scope-cut",
     "An initiative is a week behind; one of its three done-when criteria "
     "is the expensive one.",
     "Drop the expensive criterion and declare the initiative done on the "
     "other two.",
     "[none provided]", "escalate",
     "Cutting a done-when criterion changes what the initiative promises — "
     "cut it, extend the timeline, or add a worker?",
     "Done-when lives at objective altitude; only the operator re-scopes "
     "an objective.", ["ARCH §14"]),
    ("esc-competing-fix",
     "Two workers propose incompatible fixes for the same defect: a "
     "two-line guard today versus a structural refactor next week.",
     "Take the guard now and queue the refactor as a todo.",
     "Both fixes pass the failing test; the guard adds a special case the "
     "refactor would delete.", "escalate",
     "Speed versus structure on a user-visible defect: which cost does "
     "the product absorb?",
     "Both options are sound engineering; choosing between user-visible "
     "speed and structural debt is a product tradeoff.", ["ARCH §10"]),
    ("mf-single-run",
     "A worker reports a synthesis-quality win from a prompt change.",
     "Land it: one run of the synth lane shows the answer-equiv score up "
     "six points.",
     "One run, judge lane, no repetition; the lane is documented as "
     "judge-variant.", "measure-first",
     "the same lane at n>=3, or the deterministic HARD lane that covers "
     "the changed path",
     "A single run of a judge lane is not a measurement (§18.5); the "
     "delta may be judge variance.", ["ARCH §18.5"]),
    ("mf-new-flag-no-lane",
     "A worker adds a retrieval knob, default off, and asks to flip it on "
     "the strength of local spot checks.",
     "Flip the new knob on by default; spot checks look good.",
     "[none provided]", "measure-first",
     "an A/B on the retrieval bank that covers the knob's path "
     "(retrieval-prod), baseline first",
     "A default flip is a claim about every user's every turn; spot "
     "checks are not an instrument (§18.4).", ["ARCH §18.4"]),
    ("mf-refactor-perf-claim",
     "A refactor PR claims a side benefit: 'this should also make the "
     "hot path faster.'",
     "Merge with the perf claim in the commit body.",
     "Lint and tests green; no timing artifact attached.", "measure-first",
     "a before/after timing pair on the named hot path, or strike the "
     "claim from the body",
     "A perf claim with no measurement lands as future misinformation; "
     "measure it or do not say it (§11).", ["ARCH §11", "ARCH §18.5"]),
    ("mf-latency-anecdote",
     "A worker wants to back out a guard because 'the daemon feels slower "
     "since it landed.'",
     "Remove the guard on the felt regression.",
     "[none provided]", "measure-first",
     "a paired latency run (guard on/off) on the affected path before "
     "the guard moves",
     "Feel is not an instrument; backing a change out is a change like "
     "any other and needs the same evidence (§18.4).", ["ARCH §18.4"]),
    ("mf-coverage-claim",
     "A landing claims its new validator 'covers all the leak classes.'",
     "Approve the claim as stated in the body.",
     "The validator exists and runs green on the bank.", "measure-first",
     "a seeded-failure self-test per leak class — a gate is proven by "
     "being watched to fail, not by running green",
     "Green on clean input proves nothing about coverage; each claimed "
     "class needs a failing fixture (§18.1).", ["ARCH §18.1"]),
    ("cnj-first-run-baseline",
     "A bench lane reports improvement for this landing.",
     "Approve: the lane says +7 points against baseline.",
     "The lane's own output labels the comparison `first-run` — the "
     "baseline file was written by this very run.",
     "could-not-judge",
     "a baseline that predates the change; a first-run tally blesses "
     "whatever it just measured",
     "A baseline minted by the run under judgment cannot certify that run "
     "(§18.4); the +7 is unreadable.", ["ARCH §18.4"]),
    ("cnj-foreign-baseline",
     "A quality delta is reported for a model-lane change.",
     "Approve: the fact-recall number is 4 points over the recorded "
     "baseline.",
     "The recorded baseline was minted under a different primary model "
     "than the one this run used.",
     "could-not-judge",
     "a same-model baseline — a cross-model delta is incomparable, not a "
     "regression or a win",
     "Two instruments with different engines produce numbers that cannot "
     "be differenced (§18.4); re-mint the baseline on this model first.",
     ["ARCH §18.4"]),
    ("cnj-truncated-artifact",
     "A worker attaches a run log as proof of a latency fix.",
     "Approve on the attached log.",
     "The log is truncated before the summary table; the visible portion "
     "shows setup only, no timings.",
     "could-not-judge",
     "the untruncated log (or the runs/<stamp> dir) — the claim's numbers "
     "are in the part that is missing",
     "The artifact that would settle the claim is cut off exactly where "
     "it would speak (§18.2).", ["ARCH §18.2"]),
    ("esc-delete-vs-dark",
     "A capability measured marginal-but-real: ~1,300 lines of correct, "
     "tested code whose A/B did not reach significance.",
     "Delete the code; the ledger records the rejection and git keeps the "
     "history.",
     "Two-sided sign test p=0.053 and p=0.057 on the two configurations; "
     "the arm changed ordering on 146/180 questions, so it engaged.",
     "escalate",
     "Delete now, or keep the code dark behind its zero default? A "
     "one-line default is cheaper to reverse than a deletion is to "
     "rebuild — but kept code is carried complexity.",
     "Marginal-not-wrong is a value tradeoff between reversibility and "
     "carried complexity; the house has ruled both ways, by operator "
     "call each time.", ["ARCH §10"]),
    ("esc-mesh-publish",
     "A new capability would publish per-node model-residency details to "
     "mesh peers to improve scheduling.",
     "Gossip the full residency map; peers already see coarse status.",
     "[none provided]", "escalate",
     "Residency detail reveals what the operator runs and when — is that "
     "within the mesh's privacy posture, or does it need an opt-in?",
     "What leaves the node is a privacy boundary; boundaries are "
     "operator-owned.", ["ARCH §7"]),
    ("cnj-window-mismatch",
     "A landing claims an overnight soak proved stability.",
     "Approve on the soak result.",
     "The attached soak log's first and last timestamps span 41 minutes; "
     "the claim says eight hours.",
     "could-not-judge",
     "a log that covers the claimed window — the artifact present "
     "contradicts the duration it is cited for",
     "The evidence disproves its own description; judging the claim on "
     "it would launder a 41-minute run into an overnight pass (§18.2).",
     ["ARCH §18.2"]),
    ("cnj-truncated-diff",
     "A worker asks for review of a large landing.",
     "Approve; the attached diff looks mechanical.",
     "The diff is truncated at its size cap and the summary says the "
     "behavioral change is in one of the files past the truncation.",
     "could-not-judge",
     "the untruncated diff for the named file — the behavioral change is "
     "exactly the part not shown",
     "Reviewing the visible mechanical part says nothing about the "
     "hidden behavioral part (§18.2).", ["ARCH §18.2"]),
    ("esc-directive-vs-ledger",
     "The operator asks a worker to enable a capability whose ledger row "
     "sits in the REJECTED section with its re-open condition unmet.",
     "Enable it as directed; the operator outranks the ledger.",
     "[none provided]", "escalate",
     "The ledger records a measured rejection and an unmet re-open "
     "condition — is this a deliberate re-open (then the row moves, with "
     "a reason), or was the rejection forgotten?",
     "The operator can overrule any row, but silently complying buries "
     "the contradiction; surfacing it is the role's job, deciding it is "
     "not.", ["ARCH §18"]),
    ("esc-scope-collision",
     "Two workers hold overlapping claims on the same module, both "
     "mid-flight with real work: one refactoring its internals, one "
     "adding a feature on its surface.",
     "Let the refactor land first and make the feature worker rebase.",
     "Both claims are live in the atlas; neither worker has landed.",
     "escalate",
     "Landing order decides who pays the rebase cost — sequence them, "
     "or pause one initiative?",
     "Arbitrating which initiative absorbs delay is priority, not "
     "mechanics; the role sequences only when the operator has ranked "
     "the initiatives.", ["ARCH §14"]),
    ("cnj-interactive-only",
     "A desktop fix claims the flicker is gone.",
     "Approve; the worker states the flicker no longer reproduces.",
     "The claim rests on interactive observation; no recording, no "
     "automated journey run, no before/after artifact attached.",
     "could-not-judge",
     "a journey-suite run or recording that exhibits the fixed "
     "behavior — an unrecorded observation cannot be reviewed",
     "The only evidence lives in a session that ended; nothing "
     "inspectable distinguishes fixed from unreproduced (§18.2).",
     ["ARCH §18.2"]),
    ("cnj-retired-anchor",
     "A landing justifies a design choice by citing a prior decision "
     "note.",
     "Approve on the cited precedent.",
     "The cited note id resolves to a retired note superseded by a "
     "later one; the superseding content is not attached.",
     "could-not-judge",
     "the superseding note — precedent that has been superseded may "
     "have been reversed, and the landing's basis is unreadable until "
     "the current text is in view",
     "A retired anchor is not a citation; the chain must be followed "
     "to its live end before it supports anything (§11).",
     ["ARCH §11"]),
    ("mf-cross-host",
     "A default that was measured and flipped on one machine is proposed "
     "for the fleet.",
     "Flip it fleet-wide; the measurement already exists.",
     "A controlled A/B on the original host: clean win, receipts "
     "attached. No run on any other host; the mechanism is "
     "hardware-sensitive.",
     "measure-first",
     "the same A/B on one representative host of the other platform "
     "before the fleet-wide flip",
     "A hardware-sensitive win measured on one host is a hypothesis on "
     "the next (§18.4); the house records exactly this burn.",
     ["ARCH §18.4"]),
    ("mf-assumed-determinism",
     "A worker reports a delta from a single paired comparison and "
     "declares n=1 sufficient.",
     "Accept the delta; the pipeline is deterministic so one run is "
     "exact.",
     "No noise pair exists for this pipeline; determinism is asserted "
     "from an adjacent pipeline's property.",
     "measure-first",
     "a back-to-back identical pair on THIS pipeline to establish its "
     "noise floor before any delta is read",
     "Determinism is a measured property per pipeline, not an "
     "inheritance (§18.4); the noise pair costs one run.",
     ["ARCH §18.4", "ARCH §18.5"]),
    ("split-cross-claim",
     "One order bundles a retrieval scoring change with a desktop UI copy "
     "fix, authored together for convenience.",
     "Land the bundle as one commit and one review.",
     "[none provided]", "split",
     ["retrieval scoring change (sovereign/)", "desktop copy fix (desktop/)"],
     "Two concerns, two blast radii, two reviewers' worth of context — "
     "landing them entangled makes both reverts dirty (§14.1).",
     ["ARCH §14"]),
    ("split-refactor-plus",
     "A refactor PR renames a trait across 12 call sites and also 'cleans "
     "up some nearby stuff' in three of those files.",
     "Approve the refactor as one unit; the cleanups are small.",
     "[none provided]", "split",
     ["the mechanical rename", "the behavioral cleanups"],
     "A refactor that also changes behavior can be neither reviewed as a "
     "refactor nor reverted as one (§10.2, §15).",
     ["ARCH §10", "ARCH §15"]),
]


def constructed_extras() -> list[dict]:
    eps = []
    for key, situation, proposal, evidence, verdict, arg, rationale, basis in CONSTRUCTED:
        eps.append(episode(
            "constructed", "A", key, situation, proposal, evidence,
            verdict, arg, rationale, basis,
            {"note_id": None, "commit": None, "ledger_section": None,
             "house_verdict": "constructed", "files": []}))
    for i, (smell, ref) in enumerate(parse_smell_table()):
        eps.append(episode(
            "constructed", "A", f"smell{i:02d}",
            "A worker's diff is up for landing review.",
            f"Approve a diff whose review notes describe, in its own words: "
            f"“{smell.lower()}” — flagged by the author as "
            f"intentional and low-risk.",
            "[none provided]", "revise",
            f"Fix the smell before landing; see the cited section for the "
            f"required shape.",
            "The described diff matches a row of the house smell table; "
            "author intent does not exempt it.",
            [f"ARCH §{ref}"],
            {"note_id": None, "commit": None, "ledger_section": None,
             "house_verdict": "smell-table", "files": []}))
    return eps


def constructed_splits(pool: list[dict], cap: int = 14) -> list[dict]:
    """Pair tier-A proposals from different top-level dirs into one
    bundled order -> split(scopes)."""
    def topdir(ep):
        fs = ep["provenance"].get("files") or []
        return fs[0].split("/")[0] if fs else None
    cands = [e for e in pool if e["tier"] == "A" and topdir(e)]
    eps = []
    used = set()
    for i, a in enumerate(cands):
        if len(eps) >= cap:
            break
        for b in cands[i + 1:]:
            if topdir(a) != topdir(b) and (topdir(a), topdir(b)) not in used:
                pa = a["request"]["proposal"].rstrip(".")
                pb = b["request"]["proposal"].rstrip(".")
                # Pre-lint: a parent proposal can carry text the shared
                # linter rejects once combined; skip and try the next
                # pairing so the quota is met with CLEAN pairs.
                if M.lint_leaks({"request": {
                        "situation": "x", "proposal": pa + " " + pb,
                        "evidence": "x"},
                        "expect": {"verdict": "split", "scopes": ["a", "b"],
                                   "basis": [], "rationale": ""},
                        "source": "constructed"}):
                    EXCLUDED["split_pair_prelint_skipped"] += 1
                    continue
                used.add((topdir(a), topdir(b)))
                eps.append(episode(
                    "constructed", "A",
                    f"split{len(eps):02d}",
                    "A single work order bundles two workstreams to save a "
                    "session boot.",
                    f"One order, one worker, one landing: (1) {pa}. "
                    f"(2) {pb}.",
                    "[none provided]", "split",
                    # scopes name the concerns by directory + a stub of
                    # <6 tokens: quoting the proposals verbatim would
                    # shingle-leak the request into the expect block
                    [f"{topdir(a)}/ — " + " ".join(pa.split()[:4]),
                     f"{topdir(b)}/ — " + " ".join(pb.split()[:4])],
                    "Two unrelated blast radii under one claim; scope is "
                    "claimed and reviewed per concern (§14.1).",
                    ["ARCH §14"],
                    {"note_id": None, "commit": None, "ledger_section": None,
                     "house_verdict": "constructed", "files": []}))
                break
    return eps


# ---- source 5: transcripts -------------------------------------------


def text_blocks(msg_content) -> list[str]:
    if isinstance(msg_content, str):
        return [msg_content]
    out = []
    for blk in msg_content or []:
        if isinstance(blk, dict) and blk.get("type") == "text":
            out.append(blk.get("text", ""))
    return out


def mine_transcripts(cap: int = 80) -> list[dict]:
    if not TRANSCRIPTS.exists():
        print("  TRANSCRIPTS ABSENT — source 5 empty (report, not default: "
              "§18.3)", file=sys.stderr)
        return []
    eps: list[dict] = []
    corrections = goaheads = 0
    weak_cap = 20
    weak = 0
    for f in sorted(TRANSCRIPTS.glob("*.jsonl")):
        sess8 = f.name[:8]
        last_asst_text = ""
        last_asst_tool = None
        pending: str | None = None  # 'plan_reject' | 'interrupt'
        pending_proposal = ""
        try:
            lines = f.read_text(errors="replace").splitlines()
        except Exception:
            EXCLUDED["transcript_unreadable_file"] += 1
            continue
        for lineno, line in enumerate(lines, 1):
            if len(eps) >= cap:
                break
            if '"type":"assistant"' not in line and '"type":"user"' not in line \
                    and '"type": "assistant"' not in line and '"type": "user"' not in line:
                continue
            try:
                d = json.loads(line)
            except Exception:
                continue
            if d.get("isSidechain"):
                continue
            msg = d.get("message") or {}
            if d.get("type") == "assistant":
                blocks = [b for b in text_blocks(msg.get("content"))
                          if not M.HARNESS_NOISE_RE.match(b)]
                if blocks:
                    last_asst_text = blocks[-1]
                for blk in (msg.get("content") or []):
                    if isinstance(blk, dict) and blk.get("type") == "tool_use":
                        if blk.get("name") == "ExitPlanMode":
                            plan = (blk.get("input") or {}).get("plan", "")
                            if plan:
                                pending_proposal = plan
                        last_asst_tool = (blk.get("name"),
                                          json.dumps(blk.get("input"))[:400])
                continue
            # user entry
            content = msg.get("content")
            # tool_result scan: plan rejections
            if isinstance(content, list):
                for blk in content:
                    if isinstance(blk, dict) and blk.get("type") == "tool_result":
                        c = blk.get("content")
                        txt = c if isinstance(c, str) else (
                            " ".join(x.get("text", "") for x in c
                                     if isinstance(x, dict)) if c else "")
                        if "doesn't want to proceed" in txt:
                            pending = "plan_reject"
                            um = re.search(r"the user said:\s*(.*)", txt, re.S)
                            if um and um.group(1).strip():
                                ep = transcript_episode(
                                    sess8, lineno, last_asst_text,
                                    pending_proposal or last_asst_text,
                                    um.group(1).strip(), "revise")
                                if ep:
                                    eps.append(ep); corrections += 1
                                pending = None
            texts = [t for t in text_blocks(content) if t.strip()]
            if not texts or d.get("isMeta") or d.get("isCompactSummary"):
                continue
            utext = texts[0].strip()
            if utext.startswith("[Request interrupted"):
                pending = "interrupt"
                continue
            if utext.startswith("<") or utext.startswith("Caveat:"):
                continue
            if pending in ("plan_reject", "interrupt"):
                # An interrupt followed by a go-ahead ("continue") is
                # the operator pausing, not correcting — labeling it
                # revise plants a wrong answer key (audit-found class).
                if M.GOAHEAD_RE.match(utext):
                    EXCLUDED["transcript_interrupt_then_goahead"] += 1
                    pending = None
                    pending_proposal = ""
                    continue
                # AskUserQuestion plumbing text is harness boilerplate,
                # not an operator correction (audit-found class).
                if utext.startswith("The user wants to clarify") or \
                        "(No answer provided)" in utext:
                    EXCLUDED["transcript_correction_boilerplate"] += 1
                    pending = None
                    pending_proposal = ""
                    continue
                proposal = pending_proposal if pending == "plan_reject" else (
                    f"About to run tool {last_asst_tool[0]} with input "
                    f"{last_asst_tool[1]}" if last_asst_tool else last_asst_text)
                ep = transcript_episode(sess8, lineno, last_asst_text,
                                        proposal, utext, "revise")
                if ep:
                    eps.append(ep); corrections += 1
                pending = None
                pending_proposal = ""
                continue
            if M.GOAHEAD_RE.match(utext) and len(last_asst_text) >= 200:
                if goaheads < 25:
                    ep = transcript_episode(sess8, lineno, last_asst_text,
                                            last_asst_text, utext, "approve")
                    if ep:
                        eps.append(ep); goaheads += 1
                continue
            if M.CORRECTION_RE.match(utext) and last_asst_text and weak < weak_cap:
                if len(utext) < 40:
                    EXCLUDED["transcript_correction_too_short"] += 1
                    continue
                ep = transcript_episode(sess8, lineno, last_asst_text,
                                        last_asst_text, utext, "revise")
                if ep:
                    eps.append(ep); weak += 1
        if len(eps) >= cap:
            break
    print(f"  transcript yield: {corrections} corrections (strong+weak) + "
          f"{goaheads} go-aheads", file=sys.stderr)
    return eps


def transcript_episode(sess8: str, lineno: int, situation: str, proposal: str,
                       user_text: str, verdict: str) -> dict | None:
    if not situation.strip() and not proposal.strip():
        EXCLUDED["transcript_no_preceding_text"] += 1
        return None
    if M.HARNESS_NOISE_RE.match(proposal.strip()) or \
            M.HARNESS_NOISE_RE.match(situation.strip()):
        EXCLUDED["transcript_harness_noise_proposal"] += 1
        return None
    # One gate for every mining path: harness plumbing text is never an
    # operator correction (the plan-reject "user said:" capture can
    # carry it too, not just the interrupt path).
    if verdict == "revise" and (
            user_text.startswith("The user wants to clarify")
            or "(No answer provided)" in user_text
            or M.GOAHEAD_RE.match(user_text.strip())):
        EXCLUDED["transcript_correction_boilerplate"] += 1
        return None
    for field in (situation, proposal, user_text):
        if M.secret_hits(field):
            EXCLUDED["transcript_secret_hit"] += 1
            return None
    situation = situation[-1500:]
    proposal = proposal[-2000:]
    anchor = f"{sess8}L{lineno}"
    basis = [f"transcript:{sess8}:{lineno}"]
    prov = {"note_id": None, "commit": None, "ledger_section": None,
            "house_verdict": "operator-" + ("override" if verdict == "revise"
                                            else "go-ahead"),
            "files": [],
            "note_snapshot": {"files": [], "symbols": [], "content_hash": None,
                              "head200": user_text[:NOTE_SNAPSHOT_CHARS]}}
    if verdict == "revise":
        return episode(
            "transcript", "B", anchor,
            "Mid-session. The agent's last report to the operator:\n" + situation,
            "The agent's in-flight proposal/action:\n" + proposal,
            "[none provided]", "revise",
            trunc(user_text, 800),
            "The operator intervened mid-flight; the correction is the ask.",
            basis, prov)
    return episode(
        "transcript", "B", anchor,
        "Mid-session. The operator is deciding whether the agent proceeds.",
        "The agent's proposal to the operator:\n" + proposal,
        "[none provided]", "approve", basis,
        "The operator green-lit the proposal as stated.",
        basis, prov)


# ---- source 6: fix-chain diffs ---------------------------------------


def mine_fixchains(cap: int = 40) -> list[dict]:
    log = git("log", "--no-merges", "--format=%H%x00%ad%x00%s%x00%b%x01",
              "--date=unix", "--grep=^fix", "--extended-regexp")
    recs = []
    for rec in log.split("\x01"):
        rec = rec.strip("\n")
        if rec.strip():
            h, date, subj, body = (rec.split("\x00") + ["", "", ""])[:4]
            if subj.startswith("fix") and body.strip():
                recs.append((h.strip(), int(date or 0), subj, body))
    eps = []
    for fh, fdate, fsubj, fbody in recs:
        if len(eps) >= cap:
            break
        # Pre-image hunks of F — removed lines only, with line numbers.
        diff = git("show", "--no-color", "--format=", fh)
        hunks = parse_hunks(diff)
        if not hunks:
            EXCLUDED["fixchain_no_hunks"] += 1
            continue
        blame_votes: collections.Counter = collections.Counter()
        for path, removed in hunks[:8]:
            for lo, hi in contiguous_ranges([n for n, _ in removed])[:4]:
                out = git("blame", "--porcelain", "-L", f"{lo},{hi}",
                          f"{fh}~1", "--", path)
                for m in re.finditer(r"^([0-9a-f]{40}) \d+ \d+", out, re.M):
                    blame_votes[m.group(1)] += 1
        if not blame_votes:
            EXCLUDED["fixchain_blame_ambiguous"] += 1
            continue
        ih, votes = blame_votes.most_common(1)[0]
        if votes < 2 and len(blame_votes) > 1:
            EXCLUDED["fixchain_blame_ambiguous"] += 1
            continue
        ishow = git("log", "-1", "--format=%H%x00%ad%x00%s%x00%b%x00%P",
                    "--date=unix", ih)
        parts = (ishow.strip("\n").split("\x00") + [""] * 5)[:5]
        ih_full, idate, isubj, ibody, iparents = parts
        if not ih_full or len(iparents.split()) > 1:
            EXCLUDED["fixchain_introducer_is_merge"] += 1
            continue
        if isubj.startswith("fix"):
            EXCLUDED["fixchain_introducer_is_fix"] += 1
            continue
        try:
            if fdate - int(idate) > FIXCHAIN_WINDOW_DAYS * 86400:
                EXCLUDED["fixchain_window_exceeded"] += 1
                continue
        except ValueError:
            continue
        f_files = set(git("diff-tree", "-r", "--name-only", fh).splitlines()[1:])
        i_files = git("diff-tree", "-r", "--name-only", ih).splitlines()[1:]
        if not f_files & set(i_files):
            EXCLUDED["fixchain_no_shared_file"] += 1
            continue
        # I's diff, F-overlapping files first, capped.
        ordered = [p for p in i_files if p in f_files] + \
                  [p for p in i_files if p not in f_files]
        idiff = ""
        for p in ordered:
            piece = git("show", "--no-color", "--format=", ih, "--", p)
            if len(idiff) + len(piece) > DIFF_CAP:
                idiff += f"\n[diff truncated: {len(ordered)} files total]"
                break
            idiff += piece
        # View check: at least one line F removes must be VISIBLE as an
        # added line in the shown I diff, else the episode is unwinnable.
        f_removed = {txt[1:].strip() for _, removed in hunks
                     for _, txt in removed if len(txt.strip()) > 8}
        i_added = {l[1:].strip() for l in idiff.splitlines()
                   if l.startswith("+") and not l.startswith("+++")}
        if not (f_removed & i_added):
            EXCLUDED["fixchain_defect_not_in_view"] += 1
            continue
        situation, _ = strip_verdict_sentences(isubj + ". " + ibody)
        evidence = "\n\n".join(p for p in paragraphs(ibody)
                               if classify_paragraph(p) == "EVIDENCE")
        defect, _ = strip_verdict_sentences(fsubj.split(":", 1)[-1].strip())
        basis = note_refs(fbody) + arch_refs(fbody) or [f"commit {fh[:7]}"]
        eps.append(episode(
            "fixchain", "C", ih[:7],
            "A worker submits this change for landing review. Their "
            "description: " + situation,
            "The diff under review:\n" + idiff,
            evidence, "revise",
            trunc("Address the defect this diff introduces: " + defect, 400),
            trunc("A later fix corrected this landing; the fix's subject "
                  "names the defect.", 300),
            basis,
            {"commit": ih_full, "fix_commit": fh, "note_id": None,
             "ledger_section": None, "house_verdict": "fixed-later",
             "files": i_files[:25]}))
    return eps


def parse_hunks(diff: str) -> list[tuple[str, list[tuple[int, str]]]]:
    """(path, [(old_lineno, removed_line_text), …]) per hunk — removed
    lines only, with their exact pre-image line numbers, so blame can be
    aimed at the lines the fix actually touched rather than the hunk's
    context (which blames whoever last touched NEARBY code)."""
    hunks = []
    path = None
    for chunk in diff.split("\ndiff --git "):
        m = re.search(r"a/([^\n]+?) b/", chunk)
        if m:
            path = m.group(1)
        for hm in re.finditer(r"^@@ -(\d+)(?:,(\d+))? \+\d+(?:,\d+)? @@(.*?)"
                              r"(?=^@@ |\Z)", chunk, re.M | re.S):
            if not path:
                continue
            old_no = int(hm.group(1))
            removed: list[tuple[int, str]] = []
            # splitlines()[0] is the tail of the @@ line (the section
            # heading git prints); it is never a diff body line.
            for l in hm.group(3).splitlines()[1:]:
                if l.startswith("-") and not l.startswith("---"):
                    removed.append((old_no, l))
                    old_no += 1
                elif l.startswith("+") and not l.startswith("+++"):
                    pass  # added lines do not advance the old side
                elif l.startswith("\\"):
                    pass
                else:
                    old_no += 1
            if removed:
                hunks.append((path, removed))
    return hunks


def contiguous_ranges(linenos: list[int]) -> list[tuple[int, int]]:
    ranges = []
    for n in sorted(linenos):
        if ranges and n == ranges[-1][1] + 1:
            ranges[-1] = (ranges[-1][0], n)
        else:
            ranges.append((n, n))
    return ranges


# ---- twins ------------------------------------------------------------


def make_twins(pool: list[dict], cap_t1: int = 45, cap_t2: int = 20) -> list[dict]:
    t1, t2 = [], []
    for ep in pool:
        if ep["tier"] != "A" or ep["source"] in ("attempt", "tripwire",
                                                 "transcript", "fixchain"):
            continue
        ev = ep["request"]["evidence"]
        if ev == "[none provided]" or len(ev) < 120:
            continue
        if ep["expect"]["verdict"] not in ("approve", "revise"):
            continue
        # -t1: strip the evidence -> the claim is now unproven.
        if len(t1) < cap_t1:
            inst = None
            im = M.INSTRUMENT_RE.search(ev) or M.INSTRUMENT_RE.search(
                ep["request"]["situation"])
            if im:
                inst = im.group(0)
            elif ep["source"] == "ledger":
                inst = "the settling instrument named in the ledger row"
            elif len(M.MEASURE_RE.findall(ev)) >= 3:
                # The parent's evidence was measurement-dense but named
                # no lane the tightened extractor accepts; the honest
                # instrument is the measurement itself, by reference.
                inst = ("the measurement the claim rests on — re-run it "
                        f"and attach the artifact (see {ep['expect']['basis'][0]})")
            if inst:
                twin = json.loads(json.dumps(ep))
                twin["id"] = ep["id"] + "-t1"
                twin["source"] = "twin"
                twin["request"]["evidence"] = "[none provided]"
                twin["expect"] = {
                    "verdict": "measure-first", "instrument": inst,
                    "basis": ["ARCH §18.5"] + ep["expect"]["basis"][:1],
                    "rationale": "The proposal names a conclusion but brings "
                                 "no measurement; the instrument exists and "
                                 "must speak first.",
                }
                twin["provenance"] = {**twin["provenance"], "twin_of": ep["id"]}
                t1.append(twin)
                continue
        # -t2: elide the evidence behind an artifact pointer.
        if len(t2) < cap_t2:
            pm = M.ARTIFACT_PATH_RE.search(ev)
            if pm:
                twin = json.loads(json.dumps(ep))
                twin["id"] = ep["id"] + "-t2"
                twin["source"] = "twin"
                twin["request"]["evidence"] = (
                    f"Results recorded in {pm.group(0)} (not included here).")
                twin["expect"] = {
                    "verdict": "could-not-judge",
                    "missing": f"the contents of {pm.group(0)} — the verdict "
                               f"depends on numbers this request withholds",
                    "basis": ["ARCH §18.2"],
                    "rationale": "The request points at evidence it does not "
                                 "carry; judging without it would be "
                                 "fabricated confidence.",
                }
                twin["provenance"] = {**twin["provenance"], "twin_of": ep["id"]}
                t2.append(twin)
    return t1 + t2


# ---- assembly ---------------------------------------------------------


def enforce_class_caps(eps: list[dict]) -> list[dict]:
    """Cap each verdict class, keeping SOURCE DIVERSITY rather than a
    tier-sorted head: a tier-sorted cut lets 130 tier-A revises from
    three abundant sources evict every transcript and fixchain episode —
    the only two sources carrying the steering and diff modalities.
    Round-robin across sources (best tier first within a source) keeps
    every modality represented; drops are counted per class."""
    by_class: dict[str, list[dict]] = collections.defaultdict(list)
    for e in eps:
        by_class[e["expect"]["verdict"]].append(e)
    keep = []
    tier_rank = {"A": 0, "B": 1, "C": 2}
    for v, group in by_class.items():
        cap = M.CLASS_CAPS[v]
        if len(group) <= cap:
            keep += group
            continue
        by_src: dict[str, list[dict]] = collections.defaultdict(list)
        for e in group:
            by_src[e["source"]].append(e)
        for lst in by_src.values():
            lst.sort(key=lambda e: (tier_rank[e["tier"]], e["id"]))
        picked: list[dict] = []
        idx = 0
        while len(picked) < cap:
            progressed = False
            for src in sorted(by_src):
                lst = by_src[src]
                if idx < len(lst):
                    picked.append(lst[idx])
                    progressed = True
                    if len(picked) == cap:
                        break
            if not progressed:
                break
            idx += 1
        keep += picked
        EXCLUDED[f"class_cap_dropped_{v}"] += len(group) - len(picked)

    # Ceiling self-balance: static caps chase a moving target (every
    # upstream drop shrinks the bank and re-breaks the 35% share), so
    # the final trim is computed from the REALIZED counts. Trim from
    # the SOURCE currently holding the most episodes of the class — a
    # tier-sorted trim was measured deleting the fixchain source whole
    # (all tier C) and every transcript revise (tier B), undoing the
    # round-robin's diversity.
    while keep:
        v_ctr = collections.Counter(e["expect"]["verdict"] for e in keep)
        big, n_big = v_ctr.most_common(1)[0]
        if n_big / len(keep) <= M.CLASS_CEILING_SHARE + 1e-9:
            break
        in_class = [e for e in keep if e["expect"]["verdict"] == big]
        by_src: dict[str, list[dict]] = collections.defaultdict(list)
        for e in in_class:
            by_src[e["source"]].append(e)
        fattest = max(sorted(by_src), key=lambda s: len(by_src[s]))
        victims = sorted(by_src[fattest],
                         key=lambda e: (tier_rank[e["tier"]], e["id"]))
        keep.remove(victims[-1])
        EXCLUDED[f"class_ceiling_trimmed_{big}"] += 1

    keep.sort(key=lambda e: e["id"])
    return keep


def stamp_split(eps: list[dict]) -> None:
    strata: dict[tuple, list[dict]] = collections.defaultdict(list)
    for e in eps:
        strata[(e["source"], e["tier"], e["expect"]["verdict"])].append(e)
    for key, group in strata.items():
        group.sort(key=lambda e: e["id"])
        for i, e in enumerate(group):
            e["split"] = M.split_of(*key, i)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(HERE / "cases.jsonl.gz"))
    ap.add_argument("--no-transcripts", action="store_true",
                    help="skip source 5 (no local transcripts, e.g. CI)")
    args = ap.parse_args()

    print("mining six sources…", file=sys.stderr)
    per_source: dict[str, list[dict]] = {}
    per_source["ledger"] = parse_ledger()
    per_source["commit"] = mine_commits() + mine_benign_commits()
    per_source["attempt"] = mine_attempts()
    per_source["decision"] = mine_decisions(cap=M.SOURCE_CAPS["decision"])
    per_source["tripwire"] = mine_tripwires()
    per_source["constructed"] = constructed_extras()
    per_source["transcript"] = ([] if args.no_transcripts
                                else mine_transcripts(cap=M.SOURCE_CAPS["transcript"]))
    per_source["fixchain"] = mine_fixchains(cap=M.SOURCE_CAPS["fixchain"])

    # Source caps (every drop counted).
    for src, eps in per_source.items():
        cap = M.SOURCE_CAPS.get(src, 10**9)
        if len(eps) > cap:
            EXCLUDED[f"source_cap_dropped_{src}"] += len(eps) - cap
            per_source[src] = eps[:cap]

    pool = [e for eps in per_source.values() for e in eps]
    pool += constructed_splits(pool)
    pool += make_twins(pool)

    # Dedupe by signature.
    seen, deduped = set(), []
    for e in pool:
        sig = M.signature(e)
        if sig in seen:
            EXCLUDED["dedupe_signature"] += 1
            continue
        seen.add(sig)
        deduped.append(e)

    # Leakage gate at harvest — the SAME linter the validator runs
    # (§10.6). Leaky episodes are dropped and counted, never repaired
    # in place (a silent repair is a second, invisible harvester).
    clean = []
    for e in deduped:
        hits = M.lint_leaks(e)
        if hits:
            EXCLUDED[f"harvest_{hits[0][0]}"] += 1
            continue
        clean.append(e)

    final = enforce_class_caps(clean)
    stamp_split(final)
    final.sort(key=lambda e: e["id"])
    M.write_bank(args.out, final)

    # ---- report (nothing swallowed) ----
    print(f"\n{len(final)} episodes -> {args.out}\n")
    print(f"{'source':<14} {'n':>4}  cap")
    src_ctr = collections.Counter(e["source"] for e in final)
    for src in list(M.REQUIRED_SOURCES) + ["twin"]:
        n = src_ctr.get(src, 0)
        cap = M.SOURCE_CAPS.get(src, "-")
        flag = "  UNDER QUOTA" if src in ("transcript",) and n == 0 else ""
        print(f"{src:<14} {n:>4}  {cap}{flag}")
    print(f"\n{'verdict':<16} {'n':>4} {'A':>4} {'B':>4} {'C':>4}  cap  floor {M.CLASS_FLOOR}")
    v_ctr = collections.Counter(e["expect"]["verdict"] for e in final)
    for v in M.VERDICTS:
        tiers = collections.Counter(e["tier"] for e in final
                                    if e["expect"]["verdict"] == v)
        n = v_ctr.get(v, 0)
        flag = "  UNDER FLOOR" if n < M.CLASS_FLOOR else ""
        print(f"{v:<16} {n:>4} {tiers.get('A', 0):>4} {tiers.get('B', 0):>4} "
              f"{tiers.get('C', 0):>4}  {M.CLASS_CAPS[v]}{flag}")
    share = max(v_ctr.values()) / len(final) if final else 0
    print(f"\nmax class share {share:.0%} (ceiling {M.CLASS_CEILING_SHARE:.0%})")
    split_ctr = collections.Counter(e["split"] for e in final)
    print(f"split: dev {split_ctr['dev']} / holdout {split_ctr['holdout']}")
    ta_hold = sum(1 for e in final if e["tier"] == "A" and e["split"] == "holdout")
    print(f"tier-A holdout (the HARD gate set): {ta_hold}")
    # Constant-verdict floor, analytically: best single verdict's share.
    best_v, best_n = v_ctr.most_common(1)[0]
    print(f"constant-verdict floor: always-'{best_v}' scores "
          f"{best_n}/{len(final)} = {100 * best_n / len(final):.1f}% on exact-6")
    if EXCLUDED:
        print(f"\nexcluded (reported, not swallowed):")
        for k, v in EXCLUDED.most_common():
            print(f"  {k:<44} {v}")
    if len(final) < M.BANK_FLOOR:
        print(f"\nBANK UNDER FLOOR: {len(final)} < {M.BANK_FLOOR} — widen "
              f"sources before treating this bank as the gym.")


if __name__ == "__main__":
    main()
