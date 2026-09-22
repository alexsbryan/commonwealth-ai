# The ring room — run of show

**The story.** I have a screen in a room and a shared doc I want the people here
to use from their phones. I also have a film library on another machine, and a
folder of notes on a third. This is the whole demo in the order I do it. Every
step is a `svrn` command or a browser, and each is followed by **what appears**.

You do not need this repository. The contract behind the walk — the bars, and
what the stand-in measures — is at the end.

---

## 1. I load the doc on the machine in the room

```bash
svrn ring new my-doc        # once: scaffold it; I edit ./my-doc
svrn ring show my-doc        # show it on THIS machine
```

**What appears:** the doc at `http://127.0.0.1:4318/` (localhost) — my own view,
no mesh involved. (The demo's `ring-doc` is this app; `house-expenses` is the
same mechanism with a ledger's vocabulary.)

## 2. I serve it to the room

```bash
svrn ring host my-doc      # host it: declare it at the guest door, and bind the door
svrn daemon restart
```

**What appears:** the address guests will use, printed — derived from this
machine, nothing for me to type. `svrn ring serve` alone lists what the door
hosts; `--read` makes an app look-only; `--clear my-doc` stops hosting it.

If this machine is on more than one network (a room network *and* a tailnet),
only I know which one the guests are on: add `--bind <room address:port>`.

*(A member who wants the same app on another of their machines has
`svrn publish my-doc 4318` and then `svrn mesh app <me> my-doc` — same idea,
member-facing instead of guest-facing.)*

## 3. I grant access to people who just have phones

```bash
svrn mesh grant --all-apps
```

**What appears:** the link, and the QR drawn right there in the terminal —
scan it. No address, no model id, no file: `--ttl` defaults to 2 h and the grant
reaches the daemon's primary slot. `--qr-svg wall.svg` writes the file instead
for a screen; `svrn mesh grant --list` shows outstanding guests with their
expiry; `--revoke <token>` ends one early.

## 4. They edit in their mobile browser

**What appears:** they scan, type a name, and edit. On my screen within ~1.4 s
their edit reads `<name>, guest of <me>` — never the name alone. Within 5 s the
name is in the doc's roster. The mesh's member list never changes: a guest, not
a member. `svrn ring log my-doc` shows every act, in the order every machine
applies them.

## 5. I stream a film from my other machine

On the machine that has the library:

```bash
svrn mesh media origin 8096    # the port its media server answers on (Jellyfin's default)
svrn mesh media offer          # offer it; withdraw ends the offer
```

On the machine I want to watch from:

```bash
svrn mesh media <holder>       # prints a localhost URL — open it
```

**What appears:** the library on the rail as "offered to: everyone here" within
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
svrn ring checkpoint my-doc --out checkpoint.json   # on a machine that holds the ring
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
