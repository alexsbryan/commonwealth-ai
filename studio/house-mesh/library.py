#!/usr/bin/env python3
"""The house library: everyone's media servers, merged, nothing copied.

Asks every member's media server the same question through the mesh, merges the
answers by IMDb id, and prints who has what. Members who cannot answer come back
as rows saying why -- never as silence -- so a closed laptop is something you
render rather than something that breaks you.

    python3 library.py check                 # no API keys needed
    python3 library.py list
    python3 library.py play <peer> <item-id>

Rewrite this. It is an example, not a library.
"""

import collections
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

DAEMON = os.environ.get("SVRN_DAEMON", "http://127.0.0.1:9741")


def fanout(path, peers=None, headers=None, timeout_ms=10000):
    """Send one request to every member's media server. Returns rows, one per member.

    A row is {node_id, name, elapsed_ms, verdict} where verdict is:
      served      -- plus status, body, bytes, truncated
      failed      -- plus reason (machine off, server down, too slow)
      never_asked -- plus reason (offers no media origin, name matched nobody)
    """
    body = {"path": path, "headers": headers or {"Accept": "application/json"},
            "timeout_ms": timeout_ms}
    if peers:
        body["peers"] = peers
    req = urllib.request.Request(
        f"{DAEMON}/v1/mesh/media/fanout",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout_ms / 1000 + 5) as r:
            return json.load(r)["rows"]
    except urllib.error.HTTPError as e:
        # The daemon answered, so it is up -- this is not a "start it" problem.
        if e.code == 404:
            sys.exit(f"{DAEMON} has no fanout route. Your daemon is running an "
                     f"older build than your CLI:\n  svrn daemon stop && svrn daemon start")
        if e.code == 409:
            sys.exit("you're not in a mesh yet:  svrn mesh join '<the join link>'")
        sys.exit(f"daemon said {e.code}: {e.read().decode(errors='replace')[:200]}")
    except urllib.error.URLError as e:
        sys.exit(f"can't reach your daemon at {DAEMON}: {e.reason}\n"
                 f"try:  svrn daemon start")


def check():
    """Prove the mesh works, before anyone has set up an API key."""
    for row in fanout("/System/Info/Public"):
        if row["verdict"] == "served":
            info = json.loads(row["body"])
            print(f"  ok    {row['name']:12s} {info.get('ServerName','?')}"
                  f"  jellyfin {info.get('Version','?')}  {row['elapsed_ms']}ms")
        else:
            print(f"  --    {row['name']:12s} {row['verdict']}: {row['reason']}")


def library():
    """Everyone's movies, merged by IMDb id, attributed to whoever has a copy.

    One request for the whole house. You hold nobody's credentials: each
    person's daemon adds their own key on the way to their own server.
    """
    have, absent = collections.defaultdict(list), []
    rows = fanout("/Items?Recursive=true&IncludeItemTypes=Movie&fields=ProviderIds")
    for row in rows:
        if row["verdict"] != "served":
            absent.append((row["name"], row["reason"]))
            continue
        if row["status"] == 401:
            absent.append((row["name"], "their server refused - no key declared "
                                        "(see the README's authentication section)"))
            continue
        if row["truncated"]:
            print(f"  note: {row['name']}'s catalogue was cut at the 4 MiB cap")
        for item in json.loads(row["body"]).get("Items", []):
            key_id = item.get("ProviderIds", {}).get("Imdb") or item["Name"]
            have[key_id].append((row["name"], item["Id"], item["Name"]))
    return have, absent


def play(peer, item_id):
    """A localhost URL that streams it straight off that person's disk."""
    out = subprocess.run(["svrn", "mesh", "media", peer, "--json"],
                         capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(out.stderr.strip() or f"couldn't reach {peer}")
    return f"{json.loads(out.stdout)['url']}/Videos/{item_id}/stream?static=true"


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "check"
    if cmd == "check":
        check()
    elif cmd == "list":
        have, absent = library()
        for copies in sorted(have.values(), key=lambda c: c[0][2].lower()):
            print(f"  {copies[0][2][:48]:48s}  {', '.join(w for w, _, _ in copies)}")
        for name, why in absent:
            print(f"  (not asked: {name} -- {why})")
        print(f"\n  {len(have)} titles across the house")
    elif cmd == "play" and len(sys.argv) == 4:
        print(play(sys.argv[2], sys.argv[3]))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
