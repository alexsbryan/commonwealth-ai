#!/usr/bin/env python3
"""Composition as a Decider over a per-party ledger (ONTOLOGY_PRIMITIVES.md §8, Axis 5): the model proposes
one move from a set code computed, code applies it.

compose.py calls `compose_blocks` for a `[[compose]]` spec with `method = "ledger"`, in place of its
union-find over link/distinct/thread/bare-window rules. Per block (one counterparty), members are walked in
time order. State: the open transactions, each holding its deal refs, delivery points, periods and
commodities as SETS (a deal is a negotiation; its terms join by union, so they are evidence for a link,
never grounds to refuse one), its lifecycle state, members and last date. For each act, code computes:

  forced   a deal ref a transaction already holds: attach there
  legal    every transaction that holds no OTHER deal ref and whose lifecycle admits the act (a won or lost
           transaction takes confirms and talk, not a new request or offer: asking again starts a new deal)
  NEW      always legal

and decides when structure does: no legal transaction -> NEW; one legal transaction with evidence (shared
thread, shared document, an overlapping term) -> attach. Anything else is put to the model as a choice among
the legal ids and NEW (an enum: an illegal move cannot be decoded), and re-checked before it is applied.
Every decision is counted by why it was made.
"""
import collections, json

ORDER = {"lead": 0, "proposal": 1, "negotiating": 2, "won": 3, "lost": 3}
TERMINAL = {"won", "lost"}
OPENING = {"lead", "proposal"}  # moves that start a negotiation, not ones that report on it


def month(m):
    return m[:7] if m else None


class Txn:
    def __init__(self, key):
        self.key, self.members, self.refs, self.points, self.commodities = key, [], set(), set(), set()
        self.periods, self.docs, self.threads, self.state, self.last = [], set(), set(), None, ""

    def evolve(self, c, fold):
        """The fold: membership, terms by union, lifecycle as the furthest stage reached, terminal sticks."""
        at = c.get("attributes") or {}
        self.members.append(c)
        if at.get("deal_ref"):
            self.refs.add(fold(at["deal_ref"]))
        if at.get("delivery_point"):
            self.points.add(fold(at["delivery_point"]).replace(" ", ""))
        if at.get("commodity"):
            self.commodities.add(at["commodity"])
        if at.get("period_start") or at.get("period_end"):
            self.periods.append((month(at.get("period_start") or at.get("period_end")), month(at.get("period_end") or at.get("period_start"))))
        self.docs.add(c["_doc"]); self.threads.add(c["_thread"])
        st = at.get("stage")
        if st in ORDER and (self.state not in TERMINAL) and (self.state is None or ORDER[st] >= ORDER[self.state]):
            self.state = st
        self.last = max(self.last, c["_date"] or "")

    def admits(self, c):
        st = (c.get("attributes") or {}).get("stage")
        return not (self.state in TERMINAL and st in OPENING)

    def evidence(self, c, fold):
        at = c.get("attributes") or {}
        why = []
        if c["_thread"] and c["_thread"] in self.threads:
            why.append("thread")
        if c["_doc"] in self.docs:
            why.append("document")
        if at.get("delivery_point") and fold(at["delivery_point"]).replace(" ", "") in self.points:
            why.append("delivery point")
        lo, hi = month(at.get("period_start") or at.get("period_end")), month(at.get("period_end") or at.get("period_start"))
        if lo and any(max(lo, a) <= min(hi, b) for a, b in self.periods):
            why.append("period")
        return why

    def summary(self):
        quotes = [f"{(m['_date'] or '')[:10]}: {(m.get('anchor') or '')[:160]}" for m in self.members[-2:]]
        return {"refs": sorted(self.refs), "delivery points": sorted(self.points), "commodities": sorted(self.commodities),
                "stage": self.state, "last": self.last[:10], "latest": quotes}


def choose(c, legal, ask, cache, report):
    """The model picks one of the legal ids or NEW; the enum is the move set, so nothing else decodes."""
    at = c.get("attributes") or {}
    opts = {f"T{i + 1}": t for i, t in enumerate(legal)}
    act = {k: at.get(k) for k in ("deal_ref", "delivery_point", "period_start", "period_end", "commodity", "kind", "stage") if at.get(k)}
    user = ("One counterparty's open deals, and a new passage from its mail. Which deal does the passage talk "
            "about? A deal is one negotiation: its terms may cover several months, points or products. Answer "
            "NEW when the passage starts a different deal.\n\n" +
            "\n".join(f"{k}: {json.dumps(t.summary())}" for k, t in opts.items()) +
            f"\n\nNEW PASSAGE ({(c['_date'] or '')[:10]}): {(c.get('anchor') or '')[:400]}\nREAD AS: {json.dumps(act)}")
    schema = {"type": "object", "required": ["deal"], "properties": {"deal": {"type": "string", "enum": list(opts) + ["NEW"]}}}
    ans, cached = ask(cache, "You match a passage of business email to the deal it belongs to.", user, schema)
    report[f"ledger: model choice {'replayed' if cached else 'asked'}"] += 1
    return opts.get(ans.get("deal"))


def compose_blocks(blocks, fold, ask=None, cache=None, report=None):
    """blocks: block id -> members (with _date/_doc/_thread set). -> {group key: [members]}."""
    report = report if report is not None else collections.Counter()
    groups = {}
    for blk, cs in blocks.items():
        ledger = []
        for c in sorted(cs, key=lambda c: (c["_date"] or "", c["_doc"] or "", c.get("unit") or "", c["id"])):
            ref = fold((c.get("attributes") or {}).get("deal_ref") or "")
            forced = [t for t in ledger if ref and ref in t.refs]
            legal = [t for t in ledger if not (ref and t.refs and ref not in t.refs) and t.admits(c)]
            if forced:
                t, why = forced[0], "forced: a deal ref it already holds"
            elif not legal:
                t, why = None, "new: no legal transaction"
            else:
                ev = {id(t): t.evidence(c, fold) for t in legal}
                backed = [t for t in legal if ev[id(t)]]
                if len(backed) == 1:
                    t, why = backed[0], f"code: the one legal transaction with evidence ({'+'.join(ev[id(backed[0])])})"
                elif ask is None and not backed:
                    t, why = None, f"no model: new ({len(legal)} legal, none with evidence)"
                elif ask is None:
                    t, why = max(backed, key=lambda t: t.last), f"no model: the latest of {len(backed)} with evidence"
                else:
                    t = choose(c, legal, ask, cache, report)
                    why = f"model: {'attach' if t else 'new'} ({len(legal)} legal, {len(backed)} with evidence)"
                    if t is not None and not t.admits(c):  # re-checked before it is applied
                        t, why = None, "model's move refused: lifecycle"
            if t is None:
                t = Txn(("ledger", blk, len(ledger))); ledger.append(t)
            t.evolve(c, fold)
            report[f"ledger: {why}"] += 1
        for t in ledger:
            groups[("ledger", blk, min(m["id"] for m in t.members))] = t.members
            report[f"ledger: transaction ends {t.state or 'unstaged'}"] += 1
    return groups
