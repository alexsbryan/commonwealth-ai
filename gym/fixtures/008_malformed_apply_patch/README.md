# 008 — Malformed apply_patch body

**What it tests:** the model CORRECTLY pivots to apply_patch (per the
read-attractor fix landed for 007), but the apply_patch body is
malformed and codex's verifier rejects it.

**Captured from:** the second oicp-smoke run on 2026-05-13, after
fixture 007's fix made the model pivot to writing. Real codex output
contained patches like:

```
apply_patch <<'EOF'
*** Begin Patch
*** Add File: oicp-types/Cargo.toml
+[package]
+name = "oicp-types"
+version = "0.1.0"
edition = "2021"               # ← missing + prefix
description = "..."             # ← missing + prefix
license = "MIT OR Apache-2.0"   # ← missing + prefix

+[dependencies]
+serde = { ... }
EOF                             # ← missing *** End Patch
```

Two failure modes:
1. **Missing `*** End Patch`** — heredoc body ends directly with EOF
2. **Body lines missing `+` prefix** — codex's parser treats them as
   bogus hunk headers and errors

Both are emissions the frontdoor heredoc canonicalizer must repair.
The existing canonicalizer (which fixed gym 005) handles wrapper
malformation (`*** Begin Patch ***`, `*** End Patch EOF`); it doesn't
yet inject missing `*** End Patch` or repair body-line prefixes.

**Pass criteria:**
- args parses as JSON
- `args.cmd` contains `apply_patch` AND `*** Begin Patch` AND
  `*** End Patch` AND `*** Add File:`
- does NOT contain `cat /find /ls ` (model must be writing, not reading)

**Empirical baseline (pre-fix):** 0/N — canonicalizer bails when
`*** End Patch` absent, so the malformed cmd passes through and
codex rejects it.

**Investments that should move this fixture:**
- Heredoc canonicalizer accepts missing `*** End Patch` and injects it
- Body-line `+`-prefix repair inside the canonicalizer
