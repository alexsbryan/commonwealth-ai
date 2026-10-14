"""The loop's corpora: where each one's gold, message ids and one-shot reference live.

The one-shot reference is its records (runs/ceiling) scored by score.py --records, message-level like the product;
runs/ceiling/worlds/*-oneshot-score.json were scored at folder level and serve 4 fewer (66 vs 70 of 93).

The one place loop.sh, score.py's callers, misses.py and table.py learn a corpus's paths.
`python3 corpora.py <id> score-args` prints the scorer's arguments after <msgids> <out>; `<id> msgids` its message ids.
"""
import json, os, sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
BENCH = os.path.expanduser("~/.svrnmesh/bench-corpora")
WORLDS = ["world-s1", "world-s2", "world-s3", "world-s4", "world-s5"]
ALL = WORLDS + ["ward-story"]


def corpus(cid):
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
    cid, what = sys.argv[1], sys.argv[2]
    c = corpus(cid)
    if what == "score-args":
        print(" ".join(score_args(cid)))
    else:
        print(c[what])
