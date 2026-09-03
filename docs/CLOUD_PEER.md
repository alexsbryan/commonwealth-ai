# Cloud peer — renting a GPU that joins the mesh

Rent a GPU by the minute, get a daemon carrying this host's loadout, use it,
destroy it. Two modes, and the difference is what the pod may ASK the mesh for.

Validated 2026-08-29 (instance `49188146`, RTX A6000, Delaware, **$0.08 for
10m30s**): a `--mesh` pod joined Meshsonics with a fresh endpoint key and —
holding **zero corpora of its own** — answered a `sep` query with five cited
chunks served by RuggedFox, at 0.73–0.80s round trip vs 0.19–0.21s for the same
query run locally on the founder. Federation is not a degraded path: the top
scores matched the founder's own local search exactly.

This is the peer of `CLOUD_TENSOR_PEER.md`, not a replacement. That one shards
a single model's *layers* across the WAN (ggml-RPC, one round trip per token).
This one runs a whole *daemon* and federates *retrieval*.

## The whole thing, in three commands

```bash
./scripts/dev-pod.sh up --mesh     # rent + boot; prints the instance id
./scripts/dev-pod.sh tunnel        # pod :9741 -> local :9841 (blocks; & it)
./scripts/dev-pod.sh down          # leave the mesh, then destroy
```

No instance id anywhere: every verb resolves the one pod by its Vast label. Add
`&& ./scripts/dev-pod.sh check` after the tunnel if you want the gate to say so
rather than trusting it. First boot is ~5–6 minutes, nearly all of it pulling
the CUDA image and ~35 GB of models. `./scripts/dev-pod.sh plan` prints the
sizing first, costs nothing, and rents nothing.

**Billing stops on `down` and on nothing else.** `./scripts/dev-pod.sh status`
answers "is anything costing me money right now, and how much so far".

## Before the first run

- **Vast CLI + credit.** `vastai show user --raw` should print your account;
  read the `credit` field, not `balance`. The API key lives at
  `~/.config/vastai/vast_api_key` (mode 0600).
- **An SSH key registered with Vast** — `vastai show ssh-keys`; register one
  with `vastai create ssh-key "$(cat ~/.ssh/id_ed25519.pub)"`. Without it
  `tunnel` and the teardown's mesh-leave have no way in.
- **For `--mesh` only: a running local daemon in the mesh you want joined.**
  `up --mesh` reads the founder's invite from `127.0.0.1:9741` and refuses to
  rent if it cannot (no daemon, wrong mesh, still a solo mesh, or an invite
  carrying no iroh dial string). All four refusals happen while nothing is
  billing.

The GPU allowlist is a safety guard, not a preference: the image is compiled
for CUDA archs 80;86;89;90 (`sovereign/container/Containerfile.cuda`). Auto-pick
takes the cheapest offer that passes it and **refuses an unrecognised GPU**
rather than assuming it works — a Turing card is often the cheapest offer that
fits the loadout, and you would pay for a could-not-judge. `offers` still lists
every row, marked `ARCH` (wrong CUDA arch) or `VRAM` (too small for the
configured loadout), so you can name one explicitly if you know better.

## The loadout, and the box it picks

The three slots are declared once, at the top of `scripts/dev-pod.sh`, as
`slot name bytes url`. The daemon's `config.toml` is GENERATED from those
lines, so a model cannot be pulled under one name and booted under another.

Point it at different models without editing the script:

```bash
./scripts/dev-pod.sh up --loadout my-loadout.txt     # or LOADOUT_FILE=...
```

**The VRAM and disk floors of the offer search are derived from whatever
loadout is in force**, by the same estimator the daemon runs at its own boot
(`sovereign_inference::capacity`, reachable as `svrn daemon vram-plan`). So a
bigger primary searches for a bigger card on its own:

```
$ ./scripts/dev-pod.sh plan
  primary  Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf
  fast     Qwen3.5-4B-UD-Q6_K_XL.gguf
  embed    Qwen3-Embedding-0.6B-Q8_0.gguf

  slot                    weights       kv  scratch    total
  primary                   28620     3577     1000    33197
  fast                       4064      508      500     5072
  embed                       609       76      200      885
  required                  39154
  smallest card             42713 MiB (42 GB card)
  disk for the pull            55 GB
```

Those floors were hardcoded (`gpu_ram>=46`, `disk_space>=80`) until
2026-09-03. Two things were wrong with that. A larger loadout kept searching
for 48 GB boxes it could not fit, and the mismatch only surfaced after the
pull, the boot and the bill; and 46 was over-tight for the loadout it was
written for, hiding cheaper 45 GB cards that fit fine. If the estimator cannot
be reached the script REFUSES rather than falling back to a constant — a
rental sized by a stale number is the silent substitution ARCH §18.3 forbids,
and this one costs money.

**The default loadout is byte-exact with RuggedFox on all three slots**, which
is what makes a bench run on a pod comparable to baselines minted on the
founder. It carried the 27B as primary until 2026-09-03, which was NOT this
host's primary (the 35B-A3B is) — the script's own comment conceded that was
"fine for dev, NOT for judge parity". Substituting a near-neighbour model reds
HARD bench lanes for the model and reports it as a regression in whatever
change is under test.

## The two modes

|                    | default (solo island)                | `--mesh`                                  |
|--------------------|--------------------------------------|-------------------------------------------|
| Joins your mesh    | no                                   | yes, with a fresh per-rental identity      |
| Federated retrieval| no — it has no corpora and no peers  | yes — peers serve corpora it does not hold |
| Answers for peers  | no (`SOVEREIGN_DISABLE_PEER_INFERENCE=1`) | no — same flag, deliberately          |
| mDNS               | off                                  | off — a Vast box shares a LAN segment      |

Joining is about what the pod may **ask** for, never what it may be asked to
do. That is why peer inference stays disabled in both modes.

**The mode lives in the Vast label**, not a local file, so `check` and `down`
read it back and neither can run against the wrong expectation. A `down` that
guessed "solo" on a mesh pod would destroy it without leaving, stranding a live
member row on every peer.

## What `--mesh` costs you

The join link — the mesh key plus the founder's iroh dial string — is written
into the Vast onstart script, so **Vast can read it**. The blast radius is
bounded: on an encrypted mesh every peer is dialed by key, and a stranger
holding the link can join but cannot reach a corpus flagged
`query_sharing = false` (`sovereign/crates/sovereign-mesh/src/capabilities.rs`
decides that, per corpus, and it gates advertising rather than serving).

**End a `--mesh` flight by rotating the invite:**

```bash
svrn mesh rotate     # daemon must be RUNNING; no restart, no slot reload
```

Existing members keep their membership; only new joins need the new key. Verify
it took by re-reading `join_key` from `/v1/mesh/status` — rotation is the kind
of thing that used to report success and change nothing.

## Reading the result

`check` is a gate, not a printout: it exits non-zero when what the pod is doing
contradicts the label it was rented under, and it distinguishes *could not
judge* (nothing answered) from *failed*.

```
$ ./scripts/dev-pod.sh check
[dev-pod] instance 49188146 was rented in mesh mode
 9741  mesh=Meshsonics   members=2/7 peer_inflight=0/1  HOME
 9841  mesh=Meshsonics   members=2/7 peer_inflight=0/1  HOME
[dev-pod] rented members seen by Meshsonics:
  online vast-49188146
```

A solo pod must read `solo island` on 9841 and a mesh pod must read `HOME`; the
founder's port must always read `HOME`. Anything else prints `!! EXPECTED … !!`
and exits 1.

## When it goes wrong

- **`up --mesh` refuses before renting.** That is the design — read the line, it
  names which of the four conditions failed. Nothing is billing.
- **The pod boots but never joins.** `./scripts/dev-pod.sh logs` carries the
  waiter's verdict (`JOINED` / `JOIN FAILED (HTTP …)` / `JOIN DID NOT HAPPEN`).
  A pod that did not join is a solo island whatever you asked for, and no bench
  read off it means anything.
- **`down` warns that the leave did not succeed.** It destroys anyway, because
  refusing would leave the pod billing. Repair the stranded row with
  `svrn mesh forget-member <node>`.
- **A federated query returns zero hits.** Check the founder still advertises
  the corpus: `query_sharing = false` withdraws it from federated search
  entirely, and the pod then cannot see that it ever existed.

## Changing the boot script

`up --dry-run` renders and validates the boot script and rents nothing. Use it
for any edit to `onstart_script` — that block is an unquoted heredoc, so command
substitution runs at render time, and it is the one surface nobody can review
once a pod is billing.
