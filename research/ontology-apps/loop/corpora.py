"""The loop's corpora: where each one's gold, message ids and one-shot reference live.

The one-shot reference is its records (runs/ceiling) scored by score.py --records, message-level like the product;
runs/ceiling/worlds/*-oneshot-score.json were scored at folder level and serve 4 fewer (66 vs 70 of 93).

The one place loop.sh, score.py's callers, misses.py and table.py learn a corpus's paths.
`python3 corpora.py --set inner|all` prints a corpus set; `<id> score-args` the scorer's arguments after <msgids> <out>; `<id> msgids` its message ids.
"""
import hashlib, json, os, sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
BENCH = os.path.expanduser("~/.svrnmesh/bench-corpora")
WORLDS = ["world-s1", "world-s2", "world-s3", "world-s4", "world-s5"]
ALL = WORLDS + ["ward-story"]
# Corpora whose gold is a question bank, not deal records (score_bank.py): one novel read through each blind
# recipe, the first authored with the five-axis interview, the second with the question that checks it, and three
# more each written blind by one persona (generality-prereg.md, Bar R).
BANK_RECIPES = {"saltgrass-ledger": "recipe.blind.toml", "saltgrass-asked": "recipe.asked.toml",
                "saltgrass-detective": "recipe.detective.toml", "saltgrass-editor": "recipe.editor.toml",
                "saltgrass-engineer": "recipe.engineer.toml",
                # A fourth persona, development from the start: the pair saltgrass-asked/-teacher is where recipe-author
                # disagreement is diagnosed, so the held-out personas never are.
                "saltgrass-teacher": "recipe.teacher.toml"}
BANKS = list(BANK_RECIPES)
# The inner ring: 16 of the 24 deals the one-shot serves and HEAD does not (resolve-confirm), in 10 of 19 windows.
# world-s1 is the noisiest (7 of 18 deals flipped between two reads of the same code), so read it deal by deal.
INNER = ["world-s3", "ward-story", "world-s1"]
# Held out: scored, never diagnosed, so a number on them can say the system generalizes. misses.py and decisions.py
# refuse them, and table.py reports them apart from the development set. A corpus joins before its first
# diagnosis and never leaves.
HELD_OUT = ["saltgrass-ledger", "saltgrass-detective", "saltgrass-editor", "saltgrass-engineer"]


def refuse_held_out(cid, tool):
    if cid in HELD_OUT:
        sys.exit(f"{tool}: {cid} is held out (corpora.HELD_OUT): it is scored, never diagnosed")


def judge_id(sources, keys):
    """What decides a number: the scoring rule's source texts and the answer-key files it holds records to, hashed.
    score.py and score_bank.py stamp it on every score file, and table.py --diff refuses two arms whose stamps
    differ, so a judge changed after a result is applied to both arms, the history rescored, before a comparison."""
    h = hashlib.sha256()
    for s in sources: h.update(s.encode())
    for k in keys: h.update(open(k, "rb").read())
    return h.hexdigest()[:12]


def corpus(cid):
    if cid == "uv-third":  # a tracker's cases: uv-support's fixed tune third, scored by converge-s2/uv_served.py --loop
        return dict(id=cid, kind="cases", recipe=f"{REPO}/research/ontology-apps/support/recipe-third.toml",
                    gold=f"{BENCH}/uv-support/gold")
    if cid in BANK_RECIPES:
        return dict(id=cid, kind="bank", recipe=f"{REPO}/research/ontology-apps/saltgrass/{BANK_RECIPES[cid]}",
                    bank=f"{REPO}/bench/lanes/chaos_monkey/saltgrass.toml",
                    oneshot_records=f"{REPO}/runs/ceiling/saltgrass/oneshot1.json")
    if cid == "ward-story":
        return dict(id=cid, msgids=f"{REPO}/runs/slice1/instruments/ward-msgids.json", gold=f"{BENCH}/enron-ward/gold",
                    folders=["tep", "pasadena"], story=f"{BENCH}/enron-ward/story", oneshot=f"{REPO}/runs/loop/reference/ward-story.oneshot-score.json",
                    oneshot_records=f"{REPO}/runs/ceiling/ward-oneshot-flash.json")
    w = f"{BENCH}/{cid}"
    n = cid.rsplit("-s", 1)[1]
    return dict(id=cid, msgids=f"{w}/msgids.json", gold=f"{w}/gold",
                folders=json.load(open(f"{w}/manifest.json"))["score_folders"], story=f"{w}/story",
                oneshot=f"{REPO}/runs/loop/reference/{cid}.oneshot-score.json",
                oneshot_records=f"{REPO}/runs/ceiling/worlds/s{n}-oneshot-flash.json")


def score_args(cid):
    """score.py's arguments after <msgids> <out>: the corpus id, and for a world its gold dir and folders."""
    c = corpus(cid)
    return [cid] if cid == "ward-story" else [cid, c["gold"], ",".join(c["folders"])]


if __name__ == "__main__":
    if sys.argv[1] == "--set":
        print(" ".join(INNER if sys.argv[2] == "inner" else BANKS if sys.argv[2] == "banks" else ALL)); sys.exit()
    cid, what = sys.argv[1], sys.argv[2]
    c = corpus(cid)
    if what == "kind":
        print(c.get("kind", "deals"))
    elif what == "score-args":
        print(" ".join(score_args(cid)))
    else:
        print(c[what])
