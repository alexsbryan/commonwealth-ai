"""table.py <arm dir>: the arm's served counts and miss categories, appended as one row to runs/loop/journal.tsv.
table.py --line <run dir>: one corpus's summary line.
table.py --history: the journal, one line per arm.
table.py --diff <arm dir A> <arm dir B>: deal by deal, what B gains and loses against A, with the verdict.

served = one record holds the gold deal, it holds no other and no stray, and its stage is the gold stage
(score.py's report; the same rule misses.py and the one-shot comparison use).
"""
import glob, json, os, subprocess, sys
from collections import Counter
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import corpora
from misses import served, KIND

JOURNAL = f"{corpora.REPO}/runs/loop/journal.tsv"
COLS = ["finished", "arm", "head", "dirty", "asker", "init", "restore"] + corpora.ALL + ["worlds", "ward", "attached_worlds", "attached_ward", "categories", "note", "roots"]


def line(run):
    S = json.load(open(f"{run}/score.json")); s = S["summary"]
    n = sum(served(S, g) for g in S["gold"]); att = sum(u["attached"] for u in S["updates"])
    return f"served {n:>2}/{s['gold_deals']}  found {s['gold_deals_found']:>2}  split {len(s['split'])}  merged {len(s['false_merges'])}  " \
           f"right-stage {len(s['found_with_right_stage']):>2}  attached {att:>2}/{len(S['updates'])}  records {s['deal_records']}"


def resolve_line(root):
    """RESOLVE's questions over all corpora (decisions.py): decided right, and the model's own pick before any veto,
    split by whether the asked particular's own record was offered (right answer: it) or not (right answer: none)."""
    v, pick = Counter(), Counter()
    for f in glob.glob(f"{root}/*/decisions.jsonl"):
        for l in open(f):
            r = json.loads(l)
            if r["verdict"] in ("unjudged", "failed"): v[r["verdict"]] += 1; continue
            v["judged"] += 1; v["right"] += r["verdict"] in ("right_link", "right_open", "veto_saved")
            p = r.get("picked"); own = r.get("offered_right")
            pick[(own, "none" if p is None else "own" if p == r["is_"] else "other")] += 1
    if not v["judged"]: return "RESOLVE: no judged question"
    return (f"RESOLVE: {v['right']}/{v['judged']} decided right; model's own pick: own record offered -> own {pick[(True, 'own')]}, "
            f"other {pick[(True, 'other')]}, none {pick[(True, 'none')]}; not offered -> none {pick[(False, 'none')]}, "
            f"other {pick[(False, 'other')]}; failed {v['failed']}")


def arm(root):
    rows, cats, wcats, roots = {}, Counter(), Counter(), Counter()
    for run in sorted(glob.glob(f"{root}/*/")):
        cid = os.path.basename(run.rstrip("/"))
        if not os.path.exists(f"{run}/score.json"): rows[cid] = None; continue
        S = json.load(open(f"{run}/score.json"))
        rows[cid] = (sum(served(S, g) for g in S["gold"]), len(S["gold"]), sum(u["attached"] for u in S["updates"]), len(S["updates"]))
        if os.path.exists(f"{run}/misses.jsonl"):
            for l in open(f"{run}/misses.jsonl"):
                m = json.loads(l); v = m["verdict"]
                (wcats if cid == "ward-story" else cats)[v["mechanism"] + (f"[{v['referent']}]" if v.get("referent") else "")] += 1
                roots[(v.get("root", "?"), bool(m.get("oneshot_served")))] += 1
    meta = next((json.load(open(p)) for p in sorted(glob.glob(f"{root}/*/run.json"))), {})
    tot = lambda ids, k: sum(rows[i][k] for i in ids if rows.get(i))
    worlds = [i for i in corpora.WORLDS if rows.get(i)]
    print(f"arm {os.path.basename(root.rstrip('/'))}  head {meta.get('head', '?')}  dirty {meta.get('dirty', '?')}  asker {meta.get('asker', '?')}")
    print(f"  note: {meta.get('note', '')}")
    for cid in corpora.ALL:
        if cid in rows: print(f"  {cid:<11} " + (f"served {rows[cid][0]:>2}/{rows[cid][1]}  attached {rows[cid][2]:>3}/{rows[cid][3]}" if rows[cid] else "no score"))
    print(f"  worlds ({len(worlds)}) served {tot(worlds, 0)}/{tot(worlds, 1)}  attached {tot(worlds, 2)}/{tot(worlds, 3)}")
    print("  " + resolve_line(root))
    by = sorted({k for k, _ in roots}, key=lambda k: -(roots[(k, True)] + roots[(k, False)]))
    print("  roots (all corpora; the one-shot serves): " + ", ".join(f"{k} {roots[(k, True)] + roots[(k, False)]} ({roots[(k, True)]})" for k in by))
    print("  misses, worlds: " + ", ".join(f"{m} {n}" for m, n in cats.most_common()))
    print("  misses, ward:   " + ", ".join(f"{m} {n}" for m, n in wcats.most_common()))
    row = dict(meta, arm=os.path.basename(root.rstrip("/")), **{i: (f"{rows[i][0]}/{rows[i][1]}" if rows.get(i) else "") for i in corpora.ALL},
               worlds=f"{tot(worlds, 0)}/{tot(worlds, 1)}", ward=f"{rows['ward-story'][0]}/{rows['ward-story'][1]}" if rows.get("ward-story") else "",
               attached_worlds=f"{tot(worlds, 2)}/{tot(worlds, 3)}", attached_ward=f"{rows['ward-story'][2]}/{rows['ward-story'][3]}" if rows.get("ward-story") else "",
               categories=" ".join(f"{m}={n}" for m, n in (cats + wcats).most_common()),
               roots=" ".join(f"{k}={roots[(k, True)] + roots[(k, False)]}({roots[(k, True)]})" for k in by))
    new = not os.path.exists(JOURNAL)
    if not new:  # a journal begun before a column was added: its header gains it, its old rows leave it empty
        lines = open(JOURNAL).read().split("\n")
        if lines[0] != "\t".join(COLS): lines[0] = "\t".join(COLS); open(JOURNAL, "w").write("\n".join(lines))
    with open(JOURNAL, "a") as f:
        if new: f.write("\t".join(COLS) + "\n")
        f.write("\t".join(str(row.get(c, "")).replace("\t", " ").replace("\n", " ") for c in COLS) + "\n")


def verdicts(root):
    out = {}
    for f in glob.glob(f"{root}/*/score.json"):
        cid = f.split("/")[-2]; S = json.load(open(f)); mp = f.replace("score.json", "misses.jsonl")
        m = {json.loads(l)["deal"]: json.loads(l)["verdict"] for l in open(mp)} if os.path.exists(mp) else {}
        for g in S["gold"]: out[(cid, g)] = (served(S, g), m.get(g))
    return out


def diff(a, b):
    """Deal by deal: what arm b serves that a does not (with a's verdict), and what it stops serving (with b's)."""
    A, B = verdicts(a), verdicts(b)
    both = [k for k in A if k in B]
    for title, keys, src in (("gained", [k for k in both if B[k][0] and not A[k][0]], A), ("lost", [k for k in both if A[k][0] and not B[k][0]], B)):
        print(f"{title} {len(keys)}" + (" (verdict in the arm that misses it)" if keys else ""))
        for k in sorted(keys):
            v = src[k][1] or {}
            print(f"  {k[0]:<10} {k[1]:<18} {v.get('mechanism', '?'):<22} {v.get('referent') or '':<12} {v.get('detail', '')[:110]}")
    print(f"net {sum(B[k][0] for k in both) - sum(A[k][0] for k in both):+d} over {len(both)} gold deals in both arms")
    # The paired interval is world/compare_runs.py's (bootstrap over worlds, then items), on the worlds both arms scored.
    pairs = [(f"{corpora.corpus(w)['gold']}:{a}/{w}/score.json", f"{corpora.corpus(w)['gold']}:{b}/{w}/score.json")
             for w in corpora.WORLDS if os.path.exists(f"{a}/{w}/score.json") and os.path.exists(f"{b}/{w}/score.json")]
    if pairs:
        out = subprocess.run(["python3", f"{corpora.REPO}/research/ontology-apps/world/compare_runs.py", "--a", *[x for x, _ in pairs],
                              "--b", *[y for _, y in pairs]], capture_output=True, text=True)
        print("".join((out.stdout or out.stderr).splitlines(keepends=True)[:5]), end="")


if __name__ == "__main__":
    if sys.argv[1] == "--line": print(line(sys.argv[2]))
    elif sys.argv[1] == "--history":
        for r in list(open(JOURNAL))[1:]:
            x = dict(zip(COLS, r.rstrip("\n").split("\t")))
            print(f"{x['finished'][:16]}  {x['arm']:<22} {x['head']:<10} worlds {x['worlds']:<6} ward {x['ward']:<6} {x['note'][:90]}")
    elif sys.argv[1] == "--diff": diff(sys.argv[2], sys.argv[3])
    else: arm(sys.argv[1])
