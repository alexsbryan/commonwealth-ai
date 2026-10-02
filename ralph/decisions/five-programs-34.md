<!-- ledger -->

**five-programs-34 · 2026-09-24 · REVIEW-mint-fp-guest-attest · director** — this commit
- Needed: the mint halted before minting. The row said the daemon signs with "the key cw-rails already trusts through the shared `load_or_generate_node_key`". The loader is shared, but the key is not: the daemon loads from ~/.svrnmesh and rails from ~/.commonwealth-rails.
- Chose: the attester is any roster member of the namespace, verified against the roster rails already derives. The mint produces fp-72 (rail-core wire type), fp-73 (rails verifies and honours it), fp-74 (the daemon issues it and drops its 503), and REVIEW-fp54-signer-identity, which measures the fp-54 signer gap the census exposed and hands the node-identity fix to the operator if that gap is real. Boundary gate 54, unchanged.
- Because: `SignedOp::on_behalf_of` already means "a member's signature over somebody else's name means 'this door says so'", so the roster is the existing trust decider (ARCH 8, 11). A pinned-attester config key would be a second decider. One node identity is the de-embed question, and it is not this row's to settle. The choice is forward-compatible with that question: under one identity the daemon's key is rails' own self_pubkey, which is always in the roster.

<!-- appendix -->

## five-programs-34 · 2026-09-24 — guest attestation trusts the namespace roster; signer-identity gap becomes a measurement row

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpguestattest-20260924.md. Reproduced by the director at 3d609a67e:

- `git grep load_or_generate_node_key`: the daemon calls it with `&self.data_dir` (daemon.rs:1291, :1614, :3072) and rails with its own `data_dir` (commonwealth-rails/src/lib.rs:140). No launcher sets `CW_RAILS_DIR`/`--data-dir` to the svrnmesh dir (the fp-70 census, five-programs-33).
- On this host, `~/.svrnmesh/node_key` exists (32 bytes) and `~/.commonwealth-rails/` does not exist, so rails' first boot generates a fresh key.
- commonwealth-rail-core/src/lib.rs:373-381 documents `on_behalf_of` as a member's signature over another name ("this door says so"). commonwealth-rails/src/rail.rs `derive_roster` builds the namespace roster from rails' mesh and its self_pubkey.
- commonwealth-rails/src/rail.rs:209-229 still drops every `on_behalf_of`. sovereign-daemon/src/routes_rail.rs:414-431 still 503s stamped appends.
- `cargo xtask boundary-gate` (toolbox, corpus-engine/) gives `FAILED (54 violation(s))`, EXIT=1.

Options weighed: (i) one node identity (rails reads the daemon's key) makes both premises true. It touches §4 rule 1 and the two-endpoint question that REVIEW-mint-fp-app-registry-one-owner already reserves, and it is end-user-visible node identity, so it is the operator's call. (iii) a pinned attester key adds a config key, a setup step and a second trust decider. (ii) reuses the roster, adds no config, and stays correct if (i) is later chosen. Its cost is that a daemon absent from rails' roster gets a named SignerNotInRoster refusal. On a default install guest writes may therefore stay refused, with a name, until the identity question is answered. REVIEW-fp54-signer-identity measures exactly that.

The package's second question, whether the fp-54 signer gap is its own row, gets yes as a MEASUREMENT row placed after fp-74. That ordering keeps the three code rows from stalling on an operator fork. If the gap is real, the row writes NEEDS_HUMAN with options (i) and the de-embed.

Falsified if: rails and the daemon already share one key on a default install (some launcher points rails at ~/.svrnmesh), in which case option (i) was already true and fp-72..74 still stand; or if `SignedOp::on_behalf_of` is changed to mean something other than a member's say-so, in which case the roster is no longer the right trust decider.

</details>
