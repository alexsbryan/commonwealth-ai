#!/usr/bin/env python3
"""Write mailbox_providers.txt: domains whose addresses name no organization.

The list is Kikobeats/free-email-domains (MIT; HubSpot's free-email list,
maintained), pinned to one commit so a regeneration is a reviewable diff. It is
never extended from a corpus: a domain one mailbox needs is that recipe's
`exclude`, and a list grown from the examples would fit them.

    python3 build_mailbox_providers.py [--commit <sha>]
"""
import argparse
import json
import pathlib
import urllib.request

REPO = "Kikobeats/free-email-domains"
PINNED = "ea1dfe996d6ba0a9198dfd7dc1cf946e4f1e15d3"  # 2026-10-05
OUT = pathlib.Path(__file__).resolve().parent / "mailbox_providers.txt"


def fetch(commit: str, path: str) -> str:
    with urllib.request.urlopen(f"https://raw.githubusercontent.com/{REPO}/{commit}/{path}", timeout=30) as r:
        return r.read().decode()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--commit", default=PINNED)
    a = ap.parse_args()
    domains = sorted({d.strip().lower().rstrip(".") for d in json.loads(fetch(a.commit, "domains.json")) if d.strip()})
    licence = fetch(a.commit, "LICENSE.md").strip().splitlines()
    head = [
        "# Mailbox providers: an address at one of these domains, or a subdomain of one,",
        "# names no organization (ontology source projection, `domain` reader).",
        f"# From https://github.com/{REPO} at {a.commit}",
        "# Regenerate: python3 ingest/crates/sovereign-recipes/_assets/build_mailbox_providers.py",
        f"# {len(domains)} domains. Upstream licence:",
        "#",
    ] + [f"# {l}".rstrip() for l in licence]
    OUT.write_text("\n".join(head + domains) + "\n")
    print(f"{len(domains)} domains -> {OUT}")


if __name__ == "__main__":
    main()
