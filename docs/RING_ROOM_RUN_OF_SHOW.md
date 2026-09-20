# The ring room, run of show

Written 2026-09-20 at the close of ring-room rr-2's machine work, for someone
who wants to run the demo, watch it, or walk it on real machines. It covers the
room as it will be: one machine in the room, the rest of the mesh elsewhere,
and phones that are guests. The earlier three-machine sitting (rr-1) is
documented in `docs/RING_ROOM_DEMO.md`, which also carries the long account of
why one of its legs cannot pass on a CPU node; this document does not repeat
either. The order the demo serves is
`.sovereign/features/ring-room-week2/order.md`, and its Demo section is the
contract the six steps below are written from.

## What a person watches

A room with a screen on the wall. The machine driving the screen, BeefyMac, is
the only mesh member in the room. The Halo (RuggedFox) is a member somewhere
else, on a different network, reached over iroh; it holds a folder nobody else
has. LittleMac is a member somewhere else too, with a Jellyfin library. People
walk in with phones that have nothing installed.

Six things happen, and each is a bar in `quality/campaigns/ring-room.toml`:

1. **Scan to name.** The wall shows one QR. A phone scans it and a page opens,
   served by BeefyMac over the room's WiFi. The person types a name, and within
   5 s it is in the doc's roster on the wall. The mesh's member list has not
   changed: the phone is a guest, holding a bearer with a lifetime inside the
   sitting, not a member.
2. **A guest's edit says whose it is.** The person types a word into the shared
   doc. On the wall within 1.4 s: "<name>, guest of BeefyMac" — never the name
   alone, because a guest must not be mistakable for a member. The Halo's
   replica holds the act after its next sync.
3. **An answer the room had to fetch.** The person asks a question only the
   Halo's folder can answer. First token on the phone within 60 s, the citation
   names RuggedFox, and BeefyMac's decision log names which node served each
   model call and by which iroh path.
4. **A film from someone else's library.** LittleMac's owner types one verb.
   The library appears in BeefyMac's Library rail as "offered to: everyone
   here" within 30 s, and a title's first byte arrives within 5 s. Nobody typed
   a Jellyfin login. While LittleMac's owner is watching their own library the
   rail says "in use" and starts nothing. The inverse verb removes the offer
   within one gossip round.
5. **The internet goes down and the room says so.** BeefyMac's uplink is cut.
   The doc keeps working for the phones. A question that needed the Halo's
   folder comes back with the Halo's corpus named unavailable and a
   general-knowledge caveat, never a citation of RuggedFox. The uplink returns
   and within 60 s the Halo's replica is byte-equal with the wall's.
6. **Nobody became a member by accident.** The member lists on BeefyMac and
   RuggedFox before and after the sitting are identical, and BeefyMac's grant
   list shows the guests with their expiry. Membership happens only by a
   member's `introduce`; the QR mints a guest.

## The stand-in

The rehearsal runs on one host as four podman containers on two podman
networks, driven by `scripts/ring-room-demo.sh` with `RING_ROOM_TOPOLOGY=room`.
That script sources `scripts/ring-doc-demo.sh` for the node door, the podman
backend, and `up`/`down`; nothing is copied between them.

| container | stands in for | networks | what it runs |
|---|---|---|---|
| `ring-room-beefy` | BeefyMac, the wall | `room` and `uplink` | a daemon with the host's GPU render node, the guest door, the wall page |
| `ring-room-halo` | the Halo, the keeper | `uplink` only | a daemon holding the corpus the ask must cite |
| `ring-room-little` | LittleMac, the holder | `uplink` only | a daemon, with Jellyfin as a sibling container in its network namespace |
| `ring-room-phone` | a guest's handset | `room` only | a stock `node:20` image, no daemon, no key; it does everything with `fetch` against the door |

`room` is the venue's WiFi and `uplink` is its internet. Cutting `uplink` from
the wall is the venue losing its connection with the room still working, which
is what step 5 needs: the bar's goodhart clause says a cut that also takes the
WiFi tests nothing, and the instrument checks the phone still reaches the wall
during the cut.

What makes `room` a room took three attempts to get right, and `up` now proves
it rather than assuming it. The room network is created `--internal`; netavark
sets the room bridge's forwarding bit to 0 for that, and podman's `isolate`
option installs no rule at all on an internal network. `up` installs one drop
rule each way between the two bridges in the rootless network namespace, then
reads both directions on the wire before any leg runs: the keeper on the uplink
must not reach the wall's door at its room address, and the phone must. If
either reading is wrong `up` exits 3 and names which. The reasoning and the
measurements are in `ralph/DECISIONS.md` under A48.

## Running it

Builds and the demo run inside the `sovereign-vulkan` toolbox (on the host,
prefix each line with `toolbox run -c sovereign-vulkan`). The demo measures the
binary, never the tree, and refuses to run on binaries older than the code it
would be judging, so build first:

```bash
./scripts/with-cargo-lock.sh ./scripts/dev-build.sh
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh verdict all
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh down
```

`verdict all` brings the room up, runs the three legs, prints one JSON row per
bar as its last lines, and exits 0 when all six passed, 1 on any FAILED, and 4
when some row could not be judged. A single bar's id in place of `all` prints
that row alone. Under the ralph harness the same run is
`scripts/ralph-check.sh demo-bg` followed by `demo-wait`, which reports the
demo's own exit code.

A cold room run measured about 3 min 40 s after the build on this host
(run 2 of 2026-09-20: build finished 09:24:44Z, verdict archived 09:28:24Z).
The three-node regression run takes about 25 minutes, most of it its answer
leg on CPU nodes.

### What the three legs do

The legs run in the order `wall`, `film`, `offline`. A leg left out of
`RING_ROOM_LEGS_ROOM` makes its bars read COULD-NOT-JUDGE rather than passing
quietly.

**The wall leg** covers steps 1, 2, 3 and 6. It records both member lists and
the wall's own corpus list first; the corpus list is the positive control,
proof that an answer citing the Halo's folder crossed the mesh. The keeper
ingests a small folder of made-up prose generated at run time (a beekeeping
cooperative on Larkspur Lane) and shares it. The wall mints two guest grants,
each written out as an SVG QR. A first phone decodes the QR, types a name,
makes one edit and asks one question. A second phone tries the wall's own
member name first and must be refused, then proceeds under its own. The leg
then reads the grant list, both member lists again, the peer path from the
daemon's existing transport observation, the decision lines for the ask, and
the keeper's replica.

**The film leg** is step 4. The holder types `mesh media offer` with no
arguments; the verb finds the local Jellyfin and mints a read-only viewer
account itself. The clock starts when the verb returns. The wall polls
`GET /v1/mesh/media`, the same endpoint the desktop Library rail reads, until
the library is listed with its "offered to" line, then picks a title and times
the first byte through the mesh tunnel. The leg reads the declared account's
policy back from Jellyfin to confirm it cannot manage or delete, plays the
library as the holder to watch the rail say "in use" and start nothing, diffs
the holder's config to confirm the driver never edited it, and ends with
`mesh media withdraw`, timing the offer's disappearance.

**The offline leg** is step 5. It disconnects the wall from `uplink` and then
asserts that the cut is a cut, on the path the bar judges: a knowledge search
from the wall for the keeper's corpus over whatever connection the transport
already holds, then a fresh capabilities fetch to each logged peer address. A
surviving path makes the bar read COULD-NOT-JUDGE with the reason
`cut-not-a-cut` and the address named, never a pass. Inside the cut a phone
scans, names itself, edits and asks. The leg then reconnects the uplink,
re-asserts the seal, and polls both ring journals until they are byte-equal,
recording `cut_at`, `heal_at` and `converged_s`.

Every string the driver types on a person's behalf goes to a census, stamped
`install` (bring-up, before anyone walks in) or `walk` (what a person would
have done), and classified by shape: address, port, URL, config line,
credential, other. The rr-2 film bar requires the census's excluded list to be
empty: no credential anywhere in the walk, on either machine.

## Reading the result

Four verdicts exist, not two: PASSED, FAILED, COULD-NOT-JUDGE, and never-ran.
The last two make no claim, and a run whose offline leg reads COULD-NOT-JUDGE
is not a cold run.

The two cold runs that closed the machine work, 2026-09-20, on binaries built
at `001ce10bd`:

| bar | run 1 | run 2 |
|---|---|---|
| `ra-room-scan-to-name` | PASSED | PASSED |
| `ra-room-guest-edit-attributed` | PASSED | PASSED |
| `ra-room-guest-ask-served-by-the-room` | PASSED | PASSED |
| `ra-room-film-from-littlemac` | PASSED, listed 8.4 s | PASSED, listed 8.4 s |
| `ra-room-offline-room-says-so` | PASSED, converged 3 s | PASSED, converged 20 s after a 64 s cut |
| `ra-room-member-only-by-vouch` | PASSED | PASSED |

Run 2's offline row is the informative one. A 64 s cut is past the 60 s
offline threshold, so both sides marked the other Offline; an earlier run with
a cut of the same length converged in 85 s and failed. The difference is that a
peer coming back Online now wakes the ring sync instead of waiting for its
next interval (`ralph/DECISIONS.md` A50).

The member-only-by-vouch bar has two halves. The negative half, that no scan
changed the member list, is what passes today. The affirmative half, that a
phone becomes a member only after a member's introduce act, reads
COULD-NOT-JUDGE until a phone can run a node.

The regression gate is the rr-1 topology, run once with
`RING_ROOM_TOPOLOGY=three`. Its expected reading is the baseline, not five
greens: answer 0.8 FAILED, doc 1.0, film 1.0, plug-in 0.0 FAILED on
`c_answer_names` with the other three sub-legs true, nothing-typed 0. Both
reds are the CPU-node ask described in `docs/RING_ROOM_DEMO.md` part two, and
what the gate checks is that they have not moved.

### What a run leaves behind

Everything lands under `target/ring-room-rr2-demo/` and its siblings, and is
overwritten by the next run; archive what you want to keep.

| to answer | open |
|---|---|
| Did the seal hold, and what did `up` read? | `room-seal.out`, and the first lines of the `-up.log` sibling |
| What did each phone see and type? | `phone-phone.json`, `phone-phone2.json`, `phone-phone-cut.json` |
| Who served the ask, and by which path? | `wall-decisions.txt`, `wall-transport.json`, `wall-fanout.json` |
| Did membership move? | `wall-beefy-before.json` against `wall-beefy-after.json`, the same pair for halo, and `wall-grants.txt` |
| When was the library listed, played, withdrawn? | `room-film.json`, `room-offer.out`, `room-withdraw.out` |
| Was the holder's config touched? | `room-little-config.before` against `.after` |
| Was the cut a cut, and how long? | `room-cut-assert.out`, `room-cut-paths.json`, `room-offline.json` |
| What did a daemon decide, and when? | `beefy/daemon.err`, `halo/daemon.err`, `little/daemon.err` |
| What was typed on a person's behalf? | the `-typed.tsv` sibling |

The daemon logs are the ground truth when a bar surprises you. Two lines were
added during rr-2 for exactly that: the gossip merge logs which arm it took for
every member on every round, with both sides' offer view and event times, and
the ring sync logs which members a round exchanged with and which it skipped
as Offline. The first named a one-second tie that delayed a film offer by a
minute (A49); the second is what made the 85 s convergence readable (A50).

## The walk on real machines

The stand-in cannot be the last word, and the queue's final row,
`HUMAN-rr-2-the-room` in `ralph/next/ring-room-rr2/STATE.md`, is a person
walking the six steps with a real phone. What the stand-in's config tells you
about setting that up:

BeefyMac and LittleMac need to be members again first; as of 2026-09-19 the
mesh reads BeefyMac as retired and LittleMac as offline. BeefyMac goes on a
WiFi that is not the tailnet, with the Halo elsewhere, because the scan bar's
goodhart clause says a run over a VPN proves nothing about the room's WiFi.

The guest door is off unless configured. On BeefyMac, `[daemon] guest_bind`
takes the room-facing `host:port` and `[daemon] guest_page_dir` takes a copy of
`sovereign/apps/ring-doc`; the door serves that page under `/ring/`. A second
app on the same wall goes in `[daemon.guest_pages]` instead — one line per rail
namespace, `ring-doc = "/path/to/bundle"` — and each is served at
`/ring/<namespace>/`, only while a live grant names that namespace. The wall's
own screen is the member page, which `svrn ring dev ring-doc --dir
sovereign/apps/ring-doc` serves on loopback. The QR comes from the grant verb, with `--url` set to the door:

```bash
svrn mesh grant --model <id from /v1/models> --rail ring-doc --ttl 2h \
  --label wall --url http://<beefy's room address>:<port>/ring/ --qr-svg wall-qr.svg
```

On the Halo, `svrn corpus ingest <folder> --share` makes the folder answerable
from the room. On LittleMac, `svrn mesh media offer` and later
`svrn mesh media withdraw` are the holder's two verbs. `svrn mesh status` on
BeefyMac and the Halo, before and after, is step 6, and
`svrn mesh grant --list` shows the guests with their expiry.

Record each step and what you saw in the order's journal with
`scripts/co-journal.sh`. Two things the stand-in cannot tell you and the walk
will: whether the iroh path between the room and the Halo is direct or relayed
on a real venue network (the ask bar records it, and a relayed run is the one
that counts), and what a real uplink outage looks like to the phones, since a
real outage is rarely shorter than a minute.

## Known limits

The stand-in runs every node on one host clock, which is how the one-second
merge tie became the common case rather than a rare one; real machines will
see it less, and the fix does not depend on that. The room bridge's forwarding
bit was observed at 1 in a leaking run and at 0 in a sealed one, and what flips
it has not been named, which is why the seal is installed and proven rather
than trusted. The ask inside a sealed cut took about 31 s in one run against
13 to 15 s otherwise, and whether a sealed room turns a refused dial into a
timed-out one is open. `last_seen` is a whole-second clock doing duty as the
merge's ordering key; with one writer it is enough for ten-second rounds, and
it cannot order two stamps by the same holder inside one second. Each of these
is recorded with its evidence in `ralph/DECISIONS.md` A48 to A50.
