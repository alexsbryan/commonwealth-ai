# Hot-path reuse — pre-registration

**The direction is the artifact — read `quality/campaigns/hot-path-reuse.toml`
first.** Its four statements D1-D4 are what we steer by; the two falsifiers
there are subordinate and exist only to stop us lying to ourselves. This file
carries the one thing that file cannot: **what we are about to build, pinned
before we build it**, so the verdict cannot be fitted to whatever shipped.

Written 2026-08-22 at HEAD `63c72af8`. Operator decisions the same day: scope is
sovereign-only with `sovereign-cli-shared` as the contract home (no new crate);
the treatment tests **H-COST**, not H-discovery.

## The claim being tested

An agent reuses a surface when it is both in its scan window and cheaper to
write than rolling one. `sovereign_cli_shared::args` is reachable, was in the
scan window for 3 of the 5 hottest hand-rollers, and got zero voluntary
adopters in 73 days — because it converges the parse loop and leaves the struct,
the coercion and the cross-flag rules with the author.

**So the prediction is: make declaring the struct BE declaring the flags, and
uptake follows without anyone being told.** If it does not, cost was not the
blocker and the campaign says so.

## What ships — the cheaper form

Today, per command: a struct, an `ArgSpec` table that restates the same field
names, a `parse` call, and a `Parsed` → struct mapping. Four statements of one
fact.

```rust
// TODAY — 53 lines in vault_report.rs for 10 flags
struct Opts { corpus_id: Option<String>, folder: Option<PathBuf>, /* ... */ }
fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() { /* ~30 lines */ }
    /* ~15 lines of cross-flag validation */
}
```

```rust
// TARGET — the field list is the flag list
#[derive(Args, Default)]
#[args(require_one_of("corpus_id", "folder"))]
struct Opts {
    corpus_id: Option<String>,
    folder: Option<PathBuf>,
    #[arg(short = 'j')] json: bool,
}
let opts = Opts::parse(args)?;
```

Three properties are load-bearing and each is a thing the current surface does
NOT do:

1. **The field list is the flag list.** `corpus_id` ⇒ `--corpus-id`. No second
   table to keep in sync. This is where the 198 lines of loop go.
2. **Value coercion comes from the field type.** `Option<PathBuf>` parses as a
   path; `bool` is a presence flag; `Option<Mode>` needs `FromStr` and says so
   at compile time. Today the author writes every conversion by hand.
3. **Cross-flag rules are declared, not coded.** `require_one_of`,
   `conflicts_with`. `vault_report.rs` spends 15 lines on exactly three such
   rules and every hand-rolled command re-derives them.

Property 3 is the one to cut first if the derive proves expensive. Properties 1
and 2 carry the measured cost; 3 is the tail.

**Retirement is part of shipping, not after it.** `ArgSpec`/`parse` stay only as
the derive's implementation, private or clearly marked as the escape hatch for
a surface the derive cannot express. Two public ways to declare one flag surface
is worse than one awkward way — that is `hpr-cheaper`'s kill clause and it is
the direct lesson of kernel-types minting `Verdict` while ten definitions
survived.

## What is deliberately NOT built

- **No documentation, guide, or AGENTS.md entry.** Operator, 2026-08-22: "more
  documentation doesn't help us resolve what is essentially an attention
  problem." Docs are not in the scan window at the moment of decision. If
  visibility turns out to be the blocker, the levers are the code-intel return
  payload and the file's own neighbouring imports — both code, neither prose.
- **No gate, lint, or preflight that forbids hand-rolling.** Operator direction
  2026-08-20, inherited: win by being useful and better, not through force. A
  gate here would convert the experiment into its own confirmation, which is
  `hpr-unprompted`'s kill clause.
- **No conversion of the five specimens as the first move.** They are the
  frozen measurement set for `hpr-cheaper`. Converting them is how the cost
  number is *computed*, not how the bar is *met* — and they are excluded from
  `hpr-unprompted`'s numerator by construction.

## The controls, run before any bar is scored

Gate zero in the campaign file, made concrete:

| Instrument | Positive control | Negative control |
|---|---|---|
| `hpr-cost.py` | convert one specimen to the derive; the count falls by the line delta of that file, ±band | rename a field; count unchanged |
| `hpr-unprompted.py` | replay a known rung commit; it is excluded, numerator stays 0 | replay a known non-rung adoption; it counts 1 |

An instrument that cannot pass both stays `deferred` and no rung is funded
against it. `nc-redundant` would have failed its positive control in one
afternoon — delete a known near-clone, watch the number not move — and nine
waves of work were spent instead.

**Preconditions are enforced by exit code, not by intention.** Each instrument
checks its own precondition first and `exit 3` when unmet, printing the unmet
condition by name and no value. `co-lineage.py` records that as
`could-not-judge / artifact-absent`, so a bar whose treatment has not shipped
can never read `failed`. This uses the substrate's existing exit-code contract
(its docstring line 36, self-tested at line 1109) rather than adding anything.

## How this ends

Three honest endings, all of them declared now:

- **hpr-cheaper meets and hpr-unprompted meets.** Cost was the blocker, the
  mechanism model holds, and the same method extends to the next re-derived
  noun (`Report`, `Mode`, `CheckStatus`).
- **hpr-cheaper meets and hpr-unprompted stays 0.** Cost was NOT the blocker,
  and D1 becomes the live question: did agents ever see it? That is a real
  result. The campaign reports it and investigates then — it does not mandate
  adoption to rescue the number, and it does not pre-build telemetry for a
  question it has not reached.
- **hpr-cheaper does not meet.** The derive could not be made cheaper than
  hand-rolling. Then the shared surface should be deleted rather than defended,
  and the honest conclusion is that flag parsing is not where a contract pays.

A fourth ending is ruled out in advance: **no bar here is met by a rung of this
campaign converting files to make its own numerator move.** Rung commits are
excluded from `hpr-unprompted` by construction, and the five specimens are
frozen for `hpr-cheaper`.
