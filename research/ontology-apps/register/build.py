"""build.py: the register corpus (ship S4). Documents are the commits that changed five projects' canon ledgers,
one per commit, their messages as written; gold is each rule those messages name, with the status canon itself
derives today (`canon list --json`: active, proposed, superseded by, retracted, refused).

    build.py <out dir>     writes <out>/raw/documents.jsonl and <out>/gold/rules.json
"""
import datetime, json, os, re, subprocess, sys

REPOS = ["canon", "zoracite", "ersilia", "eutropia", "commonwealth-ai"]
DEV = os.path.expanduser("~/dev")
CAN = re.compile(r"can-[0-9a-f]{12}")


def commits(repo):
    fmt = "%H%x1f%aI%x1f%an%x1f%s%x1f%b%x1e"
    out = subprocess.run(["git", "-C", f"{DEV}/{repo}", "log", "--no-merges", f"--format={fmt}", "--", ".canon"],
                         capture_output=True, text=True, check=True).stdout
    for rec in filter(str.strip, out.split("\x1e")):
        h, date, author, subject, body = rec.strip("\n").split("\x1f")
        text = f"{subject}\n\n{body}".strip()
        if subject.startswith("canon") or CAN.search(text):
            yield {"id": f"{repo}-{h[:10]}", "thread": repo, "created_at": date, "author": author,
                   "title": subject, "body": text}


def rules(repo):
    d = json.loads(subprocess.run(["canon", "list", "--json"], cwd=f"{DEV}/{repo}", capture_output=True, text=True,
                                  check=True).stdout)
    return d.get("commitments", [])


def withdrawn(repo):
    """{rule id: when} for every rule a retract or revert act withdrew (`canon list` shows live and proposed only)."""
    acts = [json.loads(l) for l in subprocess.run(["canon", "log", "--json"], cwd=f"{DEV}/{repo}", capture_output=True,
                                                  text=True, check=True).stdout.splitlines() if l.strip()]
    out = {}
    for a in acts:
        if a["op"] in ("retract", "revert"):
            for t in ([a["target"]] if a.get("target") else a.get("targets", [])):
                out[t] = datetime.datetime.fromtimestamp(a["ts_unix"], datetime.timezone.utc).isoformat()
    return out, {a["id"]: a for a in acts}


def main(out):
    docs, gold = [], []
    for repo in REPOS:
        ds = list(commits(repo))
        named = set(CAN.findall("\n".join(d["body"] for d in ds)))
        gone, acts = withdrawn(repo)
        listed = {c["id"]: c for c in rules(repo)}
        for i in sorted(named):
            a = acts.get(i)
            if i in listed:
                c = listed[i]; st = c.get("status") or {}
                status, by, at = st.get("status"), st.get("by"), None
            elif a and a["op"] in ("assert", "supersede") and i in gone:
                c = {"text": a.get("text", ""), "asserted_at": a["ts_unix"], "actor": a.get("actor")}
                status, by, at = "retracted", None, gone[i]
            else:
                continue
            gold.append({"project": repo, "id": i, "text": c.get("text", ""), "status": status, "superseded_by": by,
                         "retracted_at": at, "proposed_at": datetime.datetime.fromtimestamp(
                             c["asserted_at"], datetime.timezone.utc).isoformat() if c.get("asserted_at") else None,
                         "actor": c.get("actor"), "documents": [d["id"] for d in ds if i in d["body"]]})
        docs += ds
    os.makedirs(f"{out}/raw", exist_ok=True); os.makedirs(f"{out}/gold", exist_ok=True)
    with open(f"{out}/raw/documents.jsonl", "w") as f:
        for d in sorted(docs, key=lambda d: d["created_at"]):
            f.write(json.dumps(d) + "\n")
    json.dump({"rules": gold}, open(f"{out}/gold/rules.json", "w"), indent=1)
    by = {}
    for g in gold: by[g["status"]] = by.get(g["status"], 0) + 1
    print(f"{len(docs)} documents, {sum(len(d['body']) for d in docs)} chars; {len(gold)} gold rules {by}")
    for repo in REPOS:
        print(f"  {repo}: {sum(1 for d in docs if d['thread'] == repo)} docs, {sum(1 for g in gold if g['project'] == repo)} rules")


if __name__ == "__main__":
    main(sys.argv[1])
