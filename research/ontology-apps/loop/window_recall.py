"""window_recall.py <arm>/<corpus> ...: per window of the run's cut, the gold updates whose message falls in it and
how many a statement attached (score.json's `attached`), so one window can be the innermost ring.

A message belongs to the window whose date span (the chapter title) holds its Date header; the cut is the run's own,
or with `--by <arm>` that arm's, so a read at another window size is counted on the windows it is compared against.
"""
import datetime, email.utils, json, os, re, sys

import corpora

L = os.path.join(corpora.REPO, "runs/loop")


def date_of(story, folder, file):
    for line in open(f"{story}/{folder}/{file}", errors="replace"):
        if line.startswith("Date:"):
            return email.utils.parsedate_to_datetime(line[5:].strip()).astimezone(datetime.timezone.utc)


def per_window(run, cid, cut=None):
    s = json.load(open(f"{run}/score.json"))
    spans = []
    for c in json.load(open(cut or f"{run}/chapters.json"))["chapters"]:
        lo, hi = (datetime.datetime.fromisoformat(t.replace("Z", "+00:00"))
                  for t in re.findall(r"(\d{4}-\d\d-\d\dT[\d:]+Z)", c["title"]))
        spans.append((c["id"], lo, hi))
    story, out = corpora.corpus(cid)["story"], {}
    for u in s["updates"]:
        d = date_of(story, u["folder"], u["file"])
        w = next((i for i, lo, hi in spans if lo <= d <= hi), "outside")
        t = out.setdefault(w, [0, 0]); t[0] += 1; t[1] += u["attached"]
    return out


if __name__ == "__main__":
    args, by = sys.argv[1:], None
    if args[:1] == ["--by"]:
        by, args = args[1], args[2:]
    for spec in args:
        arm, cid = spec.split("/")
        corpora.refuse_held_out(cid, "window_recall.py")
        o = per_window(f"{L}/{arm}/{cid}", cid, by and f"{L}/{by}/{cid}/chapters.json")
        print(f"{spec}: " + " ".join(f"{w} {a}/{n}" for w, (n, a) in sorted(o.items()))
              + f"  total {sum(a for _, a in o.values())}/{sum(n for n, _ in o.values())}")
