# The ring room — run of show

One screen in a room. Three member machines — one in the room, two anywhere —
and phones that are guests: nothing installed on them. This is the whole demo in
the order it happens. Every step is a `svrn` command or a browser, and each
command is followed by **what appears**.

You do not need this repository. The contract behind the walk (the bars, the
stand-in's measurements) is at the end.

---

## The cast

| role | what it is | what it does |
|---|---|---|
| **the wall** | one member machine **in the room**, with a screen | shows the QR and the shared app; serves the guest page; answers the room's questions by fetching them from the keeper |
| **the keeper** | a member machine somewhere else | holds a folder of knowledge and answers questions about it |
| **the holder** | a member machine somewhere else | holds a media library and offers it to the room |
| **the guests** | phones in the room, nothing installed | scan once, type a name, then edit / ask / watch |

## Setting the room

**On every machine, once:**

```bash
svrn setup
```

**What appears:** the hardware detected, models downloaded, a daemon running.
`svrn doctor` if anything feels off.

**One machine forms the mesh:**

```bash
svrn mesh create          # prints an invite
```

**every other machine:**

```bash
svrn mesh join <invite>
```

**What appears:** `svrn mesh status` lists the members, the models each runs,
and what knowledge each hosts.

### The wall — the machine in the room

```bash
svrn ring new my-app       # a shared app: you edit ./my-app
svrn ring dev my-app       # the wall's own screen (loopback)
svrn ring serve my-app     # host it to the room
svrn daemon restart
```

**What appears:** `ring serve` prints the address it will advertise for guests —
derived from this machine rather than typed. `svrn ring serve` alone lists what
the door hosts; `--clear my-app` stops hosting one; `--read` makes an app
look-only. **If this machine is on more than one network** (a room network *and*
a tailnet), only you know which one the guests are on: add
`--bind <room address:port>`.

**One QR for everything the wall hosts:**

```bash
svrn mesh grant --wall
```

**What appears:** the link, and the QR drawn right there in the terminal —
scan it. No address, no model id, no file: `--ttl` defaults to 2 h and the
grant reaches the daemon's primary slot. `--qr-svg wall.svg` writes the file
instead, for a screen; `svrn mesh grant --list` shows outstanding guests with
their expiry, `--revoke <token>` ends one early.

### The keeper — the machine with the folder

```bash
svrn corpus ingest ./notes --share
```

**What appears:** the folder becomes a corpus the room can ask about. `--share`
is what lets their questions reach it.

### The holder — the machine with the library

```bash
svrn mesh media origin 8096    # the port your media server answers on (Jellyfin's default)
svrn mesh media offer          # offer it; withdraw ends the offer
```

---

## The sitting — seven beats

1. **Scan and name.** A phone scans the wall's QR. **What appears:** a page,
   served by the wall; the person types a name; within 5 s it is in the wall's
   roster. The mesh's member list is unchanged — a guest, not a member.
2. **An edit that says whose it is.** They type into the app. **What appears:**
   on the wall within ~1.4 s, `<name>, guest of <wall>` — never the name alone.
   `svrn ring log <ns>` on the wall shows every act in the order every machine
   applies them.
3. **A question the room had to fetch.** They ask something only the keeper's
   folder answers. **What appears:** the first token on the phone within 60 s,
   citing the keeper. `svrn mesh transport` on the wall shows whether iroh
   carried it direct or through a relay.
4. **A film from someone else's library.** The holder offers it. **What
   appears:** the library on the wall's Library rail as "offered to: everyone
   here" within 30 s, and a title plays with no login typed (first byte within
   5 s). The desktop opens a title in the browser; `svrn mesh media <holder>`
   prints the same URL for any player. While the holder is watching it
   themselves, the rail says "in use" and starts nothing.
5. **The internet goes down (the offline leg).** Cut the wall's uplink, leaving
   its WiFi up. **What appears:** the app keeps working on the phones; a
   question needing the keeper's folder comes back saying it is unavailable,
   with no citation of the keeper. Reconnect: the two replicas are byte-equal
   within 60 s.
6. **Nobody became a member by accident.** **What appears:** `svrn mesh status`
   on the wall and the keeper is unchanged, and `svrn mesh grant --list` shows
   the guests with their expiry.
7. **The record survives the cut (the checkpoint leg).** The wall freezes its
   record while the room is cut off; after the heal, a machine that was never
   asked verifies it cold, and three forgeries are refused by name. See below.

## The record, carried

Where the ring is held:

```bash
svrn ring checkpoint <namespace> --out checkpoint.json
```

Carry the file anywhere (AirDrop, USB, email to yourself) — even to a machine
the mesh has never seen:

```bash
svrn ring checkpoint --verify checkpoint.json
```

**What appears:** exit 0, the document's marks, and `verified — N admitted
act(s) … no gaps`. One flipped byte, a truncated tail, or a repeated sequence
number is refused by name with the failing step and the actor. `--roster <file>`
verifies against the verifier's own roster instead of the document's.

## The guest who is not in the room

A plain mobile browser on a network the wall has never seen still reaches it:
the page loads from one static origin, then dials the wall itself over iroh's
relay. Nothing of yours is exposed — no port, no tunnel, no tailnet — and
nothing proxies HTTP. (Measured WORKED at the pinned iroh:
`ralph/DECISIONS.md` browser-dial-2.)

```bash
svrn mesh grant --wall --url https://svrnme.sh/     # the page's origin, not the door
```

**What appears:** the QR again, and its link now carries the wall's dial string
(`iroh=`) beside the token, so the guest's browser connects from anywhere. The
page is `sovereign/apps/ring-runtime`, shipped with the landing deploy
(`landing/scripts/build-ring-runtime.sh`, run by `npm run deploy`) and served at
`https://svrnme.sh/ring/`.

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

**The rr-1 regression**, `RING_ROOM_TOPOLOGY=three`, is run once as a gate and
has **two expected reds** at its long-standing baseline — answer 0.8 and
plug-in 0.0 on `c_answer_names`, both the CPU-node ask; what the gate checks is
that they have not moved (`docs/RING_ROOM_DEMO.md` has the account).

## Fine print

- The stand-in runs every node on one host clock, which is how the one-second
  merge tie became common rather than rare; real machines will see it less.
- The room-to-keeper path in a real venue is direct or relayed, and a relayed
  run is the one that counts — the ask bar records which.
- Grants live in memory by design: a daemon restart drops every outstanding
  one, and the holder then gets a 401.
- The guest's floor on a sealed cut: the ask read about 31 s against 13–15 s
  otherwise. The seal is installed and proven rather than trusted.
- `ra-room-member-only-by-vouch`'s affirmative half (a phone becomes a member
  by voucher) reads COULD-NOT-JUDGE until a phone can run a node.

The measurements behind each of these are in `ralph/DECISIONS.md` (A48–A50, and
each campaign's own entries).
