# The ring room — run of show

**The story.** I have a screen in a room and a shared doc I want the people here
to use from their phones. I also have a film library on another machine, and a
folder of notes on a third. This is the whole demo in the order I do it. Every
step is a `svrn` command or a browser, and each is followed by **what appears**.

You do not need this repository. The contract behind the walk — the bars, and
what the stand-in measures — is at the end.

---

## 1. I run the doc on the machine in the room

The collaborative doc app lives in this repo — I run it, I don't generate it:

```bash
svrn ring show ring-doc --dir sovereign/apps/ring-doc
```

**What appears:** the doc at `http://127.0.0.1:4318/` (localhost), plus, above
it, `rail: http://127.0.0.1:9743` — the journal this page appends its acts to,
and the namespace `ring-doc` it writes them under. The grant that lets this
process write is held only while it runs, so closing the window ends my own
write access too.

`svrn ring new <dir>` scaffolds that same shape with a ledger's vocabulary
(`house-expenses` is the repo's worked version of it). It is for an app of my
own, and it is why this walk runs `ring-doc` rather than generating a directory:
a fresh `svrn ring new my-doc` is the expenses app, not the doc.

## 2. I share it on the rail

```bash
svrn publish ring-doc 4318                                                # members reach it by key
svrn ring roster add <person> --key <their-node-pubkey> --ring ring-doc   # and their acts are theirs
```

**What appears:** the app registered under its name, then one roster row —
the person and the key their acts will be signed with. A member can now open
the doc from their own machine and write into the same `ring-doc` namespace as
me, in one order. The roster row is the load-bearing half: an act signed by a
key no roster claims is a gap in the journal, not somebody's edit.

*(Nothing is set per machine: the door's listen address DEFAULTS to
`0.0.0.0:9744` while a grant is live — it opens when the first grant is minted
and closes when the last one is revoked or expires, because a grant is the
authorization. `[daemon] guest_bind` in `~/.svrnmesh/config.toml` (written by
`svrn ring host <ns> --dir <bundle> --bind <addr:port>`) is the OVERRIDE, not a
prerequisite; the ADDRESS a guest link carries is derived per host, so a
wildcard bind is never advertised. Every app after the first is `publish` then
a grant; the door reads the same registration and needs nothing declared per
app.)*

## 3. I grant access to people who just have phones

```bash
svrn mesh grant --app ring-doc   # one app, one link
```

**What appears:** the link, and the QR drawn right there in the terminal —
scan it. The grant NAMES `ring-doc`, so that is the only app it reaches; the door
proxies to the published port, which is why hosting for guests needed no second
copy of the app. No address, no model id, no file: `--ttl` defaults to 2 h and
the grant reaches the daemon's primary slot. `--qr-svg wall.svg` writes the file
instead for a screen; `svrn mesh grant --list` shows outstanding guests with
their expiry; `--revoke <token>` ends one early.

`svrn mesh grant --all-apps` is the other shape: one link for everything
registered to guests in `[daemon.guest_pages]`, the phone landing on the door's
index to pick. Several grants can be live at once — one per app, or several
links to the same app with different labels and expiries.

**One live app is served at the door's root** (`http://<door>/`), so the app's
own absolute paths (`/__ring_dev.js`, `/assets/…`) resolve. With more than one
app live, each is served under `/ring/<name>/` instead — so an app you mean to
reach alongside another has to be built path-relative (`base: './'`).

## 4. They edit in their mobile browser

**What appears:** they scan, type a name, and edit. On my screen within ~1.4 s
their edit reads `<name>, guest of <me>` — never the name alone. Within 5 s the
name is in the doc's roster. The mesh's member list never changes: a guest, not
a member. `svrn ring log ring-doc` shows every act, in the order every machine
applies them.

## 5. I stream a film from my other machine

On the machine that has the library:

```bash
svrn mesh media origin 8096    # the port its media server answers on (Jellyfin's default)
svrn mesh media offer          # offer it; withdraw ends the offer
```

On the machine I want to watch from:

```bash
svrn mesh media                 # who offers one, right now — pick a name
svrn mesh media <member>        # a member name, or a node-id prefix (≥4 chars)
```

**What appears:** first a list — one block per member offering a media origin,
with its name, node id, status and the path currently offered, and the line
"Play one: `svrn mesh media <peer>`". Then, on the second command, a localhost
URL to open. Names come from the roster (`svrn mesh status` prints both name and
node id); a member that declares no `media_origin` never appears, and asking for
one that doesn't offer closes the dial with a sentence rather than a timeout.

The library appears on the rail as "offered to: everyone here" within
30 s; the title plays with no login typed (first byte within 5 s). The desktop's
Library rail does the same thing with a click. While the holder is watching it
themselves, the rail says "in use" and starts nothing.

## 6. I make a chat ask

```bash
svrn chat ask "How much did the Larkspur Lane honey harvest weigh?"
```

**What appears:** the answer streams with its provenance. If the folder that
answers it is on my other machine, the sources name that machine. (A guest asks
through the wall's page instead — same room, no CLI.)

## 7. The uplink drops (the offline leg)

Cut the wall's uplink, leaving its WiFi up.

**What appears:** the doc keeps working on the phones. The ask comes back saying
that folder is unavailable, with no citation of the machine holding it. Reconnect:
the two replicas are byte-equal within 60 s.

## 8. The record survives the cut (the checkpoint leg)

```bash
svrn ring checkpoint ring-doc --out checkpoint.json   # on a machine that holds the ring
# carry the file anywhere — AirDrop, USB, email to yourself — even to a machine
# the mesh has never seen:
svrn ring checkpoint --verify checkpoint.json
```

**What appears:** exit 0, the document's marks, and `verified — N admitted
act(s) … no gaps`. One flipped byte, a truncated tail, or a repeated sequence
number is refused by name, with the failing step and the actor.

## 9. A guest who is not in the room

```bash
svrn mesh grant --all-apps --url https://svrnme.sh/    # the page's origin, not the door
```

**What appears:** the QR again, and its link now carries the wall's dial string
(`iroh=`) beside the token — so a plain phone browser on a network the wall has
never seen still reaches it. No port of mine is exposed, no tunnel, no tailnet,
and nothing proxies HTTP. (Measured WORKED at the pinned iroh:
`ralph/DECISIONS.md` browser-dial-2.) The page is
`sovereign/apps/ring-runtime`, shipped with the landing deploy
(`landing/scripts/build-ring-runtime.sh`, run by `npm run deploy`) and served at
`https://svrnme.sh/ring/`.

---

## Before the story: the machines

| role | what it is | what it does |
|---|---|---|
| **the wall** | the member machine **in the room**, with the screen | shows the doc; serves the guest page; answers the room's questions by fetching them from the keeper |
| **the keeper** | a member machine somewhere else | holds the folder of notes and answers questions about it |
| **the holder** | a member machine somewhere else | holds the film library and offers it |
| **the guests** | phones in the room, nothing installed | scan once, type a name, then edit / ask / watch |

On every machine, once:

```bash
svrn setup                 # hardware, models, daemon
```

One machine forms the mesh:

```bash
svrn mesh create           # prints an invite
```

every other:

```bash
svrn mesh join <invite>
```

The keeper holds the notes:

```bash
svrn corpus ingest ./notes --share    # --share is what lets the room's questions reach it
```

**What appears:** `svrn mesh status` lists the members, the models each runs,
and what knowledge each hosts.

---

## Running it — the rehearsal on one machine

The stand-in stands four podman containers in for the machines and judges every
check with no hardware. From a repo checkout, in the `sovereign-vulkan` toolbox:

```bash
./scripts/with-cargo-lock.sh ./scripts/dev-build.sh
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh verdict all
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh down
```

**What appears:** fourteen rows — twelve PASSED and two COULD-NOT-JUDGE — and
exit **4**, which is the passing shape, not a failure (the two abstain because
their proofs are cargo tests and the decisions ledger, not the room). Under the
ralph harness the same run is `scripts/ralph-check.sh demo-bg
scripts/ring-room-demo.sh` then `demo-wait`.

**The rr-1 regression**, `RING_ROOM_TOPOLOGY=three`, is run once as a gate and has
**two expected reds** at its long-standing baseline — answer 0.8 and plug-in 0.0
on `c_answer_names`, both the CPU-node ask; what the gate checks is that they
have not moved (`docs/RING_ROOM_DEMO.md` has the account).

## Fine print

- The stand-in runs every node on one host clock, which is how the one-second
  merge tie became common rather than rare; real machines will see it less.
- The room-to-keeper path in a real venue is direct or relayed, and a relayed
  run is the one that counts — the ask bar records which.
- Grants live in memory by design: a daemon restart drops every outstanding one,
  and the holder then gets a 401.
- The guest's floor on a sealed cut: the ask read about 31 s against 13–15 s
  otherwise. The seal is installed and proven rather than trusted.
- `ra-room-member-only-by-vouch`'s affirmative half (a phone becomes a member by
  voucher) reads COULD-NOT-JUDGE until a phone can run a node.

The measurements behind each of these are in `ralph/DECISIONS.md` (A48–A50, and
each campaign's own entries).
