#!/usr/bin/env python3
"""GROUP as an online FOLD (ONTOLOGY_PRIMITIVES.md §8, Axis 5): a composed type's members, walked per block in
clock order, each joining an open instance or starting one, with the instance's state folded through a
protocol from the registry. Nothing here names a type, an attribute or a state: the [[compose]] spec, its
[shape] and its [protocol.<name>] carry them.

compose.py calls `compose_blocks` for a spec with `method = "ledger"`, in place of its union-find. An instance
holds each term (the spec's `link` attributes other than the anchor) as a SET: a composed thing is a run of
acts, and its terms join by union, so they are evidence for a join, never grounds to refuse one. Its state is
the protocol's furthest state reached, and a terminal state sticks. For each member, code computes:

  forced   an instance already holding the member's anchor value: join it
  legal    every instance holding no OTHER anchor value whose state admits the member (a terminal instance
           takes no initiating move: asking again starts a new instance)
  NEW      always legal

A member whose `status` (the spec's information-status attribute: is the composed particular new to the
discourse, or given?) is read is decided by it after a forcing anchor: new -> NEW; given -> the latest legal
instance with evidence, else the latest legal one. Otherwise structure decides: no legal instance -> NEW; one
legal instance with evidence (a shared thread, a shared document, a term in common) -> join it; several with
evidence -> the latest; none -> NEW. Every decision is counted by why it was made.
"""
import collections


class Protocol:
    """[protocol.<name>]: ordered moves, the state each produces, which moves end an instance and which open one."""

    def __init__(self, decl):
        self.rank = {}
        for i, m in enumerate(decl["order"]):
            self.rank.setdefault(decl["state"][m], i)
        self.terminal = {decl["state"][m] for m in decl["terminal"]}
        self.initiating = {decl["state"][m] for m in decl["initiating"]}

    def step(self, state, new):
        """The fold: the furthest state reached; a terminal state sticks."""
        if new not in self.rank or state in self.terminal or (state is not None and self.rank[new] < self.rank[state]):
            return state
        return new

    def admits(self, state, new):
        return not (state in self.terminal and new in self.initiating)


class Terms:
    """The spec's terms, compared by their [shape]: a range overlaps, anything else is equal once folded."""

    def __init__(self, spec, shape, fold):
        self.anchor, self.fold = spec.get("anchor"), fold
        self.names = [n for n in spec.get("link", []) if n != self.anchor]
        self.ranges = {n for n in self.names if shape.get(n, {}).get("kind", "").endswith("range")}

    def read(self, at):
        out = {}
        for n in self.names:
            if n in self.ranges:
                lo, hi = (at.get(f"{n}_start") or at.get(f"{n}_end") or "")[:7], (at.get(f"{n}_end") or at.get(f"{n}_start") or "")[:7]
                if lo:
                    out[n] = (lo, hi)
            elif at.get(n):
                out[n] = self.fold(at[n]).replace(" ", "")
        return out

    def shared(self, mine, held):
        return [n for n, v in mine.items() if n in held and (
            any(max(v[0], a) <= min(v[1], b) for a, b in held[n]) if n in self.ranges else v in held[n])]


class Instance:
    def __init__(self):
        self.members, self.anchors, self.terms = [], set(), collections.defaultdict(set)
        self.docs, self.threads, self.state, self.last = set(), set(), None, ""


def compose_blocks(blocks, spec, protocol, shape, fold, report=None, trace=None, override=None):
    """blocks: block id -> members (with _date/_doc/_thread set). -> {group key: [members]}. `trace`, when a
    list, takes one row per member: the instances it could have joined and the one it did, by position.
    `override(c, moves, t, why) -> (t, why)` sees every decision before it is applied (oracle.py's seam)."""
    report = report if report is not None else collections.Counter()
    proto, terms = Protocol(protocol), Terms(spec, shape, fold)
    move_attr, status_attr, anchor = protocol["attribute"], spec.get("status"), spec.get("anchor")
    groups = {}
    for blk, cs in blocks.items():
        ledger = []
        for c in sorted(cs, key=lambda c: (c["_date"] or "", c["_doc"] or "", c.get("unit") or "", c["id"])):
            at = c.get("attributes") or {}
            ref, move, mine = fold(at.get(anchor) or "") if anchor else "", at.get(move_attr), terms.read(at)

            def evidence(t):
                why = (["thread"] if c["_thread"] and c["_thread"] in t.threads else []) + (["document"] if c["_doc"] in t.docs else [])
                return why + terms.shared(mine, t.terms)
            forced = [t for t in ledger if ref and ref in t.anchors]
            legal = [t for t in ledger if not (ref and t.anchors and ref not in t.anchors) and proto.admits(t.state, move)]
            ev = {id(t): evidence(t) for t in legal}
            backed = [t for t in legal if ev[id(t)]]
            status = at.get(status_attr) if status_attr else None
            if forced:
                t, why = forced[0], "forced: an anchor it already holds"
            elif status == "new":
                t, why = None, "status new: new"
            elif status == "given" and legal:
                t = max(backed or legal, key=lambda t: t.last)
                why = f"status given: the latest of {len(backed)} with evidence" if backed else f"status given: the latest of {len(legal)} legal"
            elif not legal:
                t, why = None, "new: no legal instance"
            elif len(backed) == 1:
                t, why = backed[0], f"code: the one legal instance with evidence ({'+'.join(ev[id(backed[0])])})"
            elif not backed:
                t, why = None, f"new: {len(legal)} legal, none with evidence"
            else:
                t, why = max(backed, key=lambda t: t.last), f"code: the latest of {len(backed)} with evidence"
            if override is not None:
                t, why = override(c, forced or legal, t, why)
            if trace is not None:
                pos = {id(x): i for i, x in enumerate(ledger)}
                trace.append({"act": c["id"], "block": blk, "why": why, "to": pos[id(t)] if t else None, "before": len(ledger),
                              "forced": [pos[id(x)] for x in forced], "legal": [pos[id(x)] for x in legal],
                              "evidence": {pos[id(x)]: ev[id(x)] for x in legal if ev[id(x)]},
                              "illegal": {pos[id(x)]: "anchor" if ref and x.anchors and ref not in x.anchors else "protocol"
                                          for x in ledger if x not in legal}})
            if t is None:
                t = Instance(); ledger.append(t)
            t.members.append(c)
            if ref:
                t.anchors.add(ref)
            for n, v in mine.items():
                t.terms[n].add(v)
            t.docs.add(c["_doc"]); t.threads.add(c["_thread"])
            t.state = proto.step(t.state, move)
            t.last = max(t.last, c["_date"] or "")
            report[f"ledger: {why}"] += 1
        for t in ledger:
            groups[("ledger", blk, min(m["id"] for m in t.members))] = t.members
            report[f"ledger: instance ends {t.state or 'unstaged'}"] += 1
    return groups
