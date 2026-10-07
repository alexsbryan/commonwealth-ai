#!/usr/bin/env python3
"""Kim Ward's Enron mailbox (CMU release, maildir/ward-k) as the CRM proof's corpus.

    prepare.py [--src ~/.sovereign/corpora-staging/enron/maildir/ward-k]
               [--out ~/.svrnmesh/bench-corpora/enron-ward] [--sample 1000] [--seed 7]

Ward's folders overlap (all_documents, sent and inbox repeat the customer folders), so messages are
deduplicated by content: (Date, From, Subject, whitespace-folded body). Not by Message-ID — the CMU
release stamps every FILE with its own id, so a message filed twice carries two (2,611 files, 2,611
ids, 2,044 messages). The release also drops In-Reply-To and References. The kept copy is the
one in a named folder over a generic one, because the customer folders (svrn-docs/ontology-apps/README.md: citizens_utilities, palo_alto, ...) label
the company dimension for free. Bytes are copied unchanged; the `email` extractor parses any file.

Writes, all outside git (personal data):
  <out>/all/<folder>/<n>     every unique message
  <out>/sample/<folder>/<n>  every unique message in a CUSTOMER folder, then a seeded random fill
                             to --sample from the rest: one build serves the cost/yield baseline and
                             the records gold
  <out>/manifest.json        per message: id, kept path, every folder it appeared in, date, in sample
"""
import argparse, collections, email, email.policy, hashlib, json, pathlib, random, shutil

GENERIC = {"all_documents", "sent", "sent_items", "_sent_mail", "inbox", "discussion_threads",
           "notes_inbox", "deleted_items", "calendar", "contacts", "to_do"}
CUSTOMER = {"citizens_utilities", "gas_customers___chris_foster", "palo_alto", "smurfit", "pasadena",
            "mesa", "tep", "el_paso_electric", "smud", "bhp"}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    home = pathlib.Path.home() / ".svrnmesh/bench-corpora"
    ap.add_argument("--src", type=pathlib.Path,  # the staged CMU maildir enron-sample-onemailbox reads
                    default=pathlib.Path.home() / ".sovereign/corpora-staging/enron/maildir/ward-k")
    ap.add_argument("--out", type=pathlib.Path, default=home / "enron-ward")
    ap.add_argument("--sample", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()

    seen = collections.defaultdict(list)          # content key -> [(folder, path, date, message id)]
    for p in sorted(x for x in a.src.rglob("*") if x.is_file()):
        m = email.message_from_bytes(p.read_bytes(), policy=email.policy.compat32)
        body = "" if m.is_multipart() else str(m.get_payload())
        key = "|".join([m.get("Date") or "", m.get("From") or "", m.get("Subject") or "",
                        hashlib.sha1(" ".join(body.split()).encode()).hexdigest()])
        seen[key].append((str(p.parent.relative_to(a.src)), p, m.get("Date"), m.get("Message-ID")))

    def keep(copies):  # a named folder over a generic one; then the first by path
        named = [c for c in copies if c[0].split("/")[0] not in GENERIC]
        return (named or copies)[0]

    kept = {mid: keep(c) for mid, c in seen.items()}
    customer = sorted(m for m, (f, *_) in kept.items() if f.split("/")[0] in CUSTOMER)
    rest = sorted(m for m in kept if m not in set(customer))
    fill = random.Random(a.seed).sample(rest, max(0, min(len(rest), a.sample - len(customer))))
    sample = set(customer) | set(fill)

    for d in ("all", "sample"):
        shutil.rmtree(a.out / d, ignore_errors=True)
    manifest = []
    for mid, (folder, p, date, _) in sorted(kept.items(), key=lambda kv: str(kv[1][1])):
        rel = pathlib.Path(folder) / p.name
        for d in ("all", "sample") if mid in sample else ("all",):
            (a.out / d / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(p, a.out / d / rel)
        manifest.append({"path": str(rel), "message_ids": [c[3] for c in seen[mid]],
                         "folders": sorted({c[0] for c in seen[mid]}),
                         "date": date, "sample": mid in sample, "customer": mid in set(customer)})
    (a.out / "manifest.json").write_text(json.dumps(manifest, indent=1) + "\n")
    files = sum(len(c) for c in seen.values())
    print(json.dumps({"files": files, "unique": len(kept), "customer_unique": len(customer),
                      "sample": len(sample), "folders": len({c[0] for c in kept.values()})}))


if __name__ == "__main__":
    main()
