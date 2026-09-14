# SCIP/daemon/fieldglass are generalized products — never encode this machine's layout into product code

Operator steer (2026-08-06, during the SCIP-reindexer PATH fix): "SCIP, the daemon, etc are generalized products that need to work in the whole universe of contexts they'll be deployed in. I worry you're overfitting to the home context."

Why: A fix that enumerates this host's layout (`~/.cargo/bin`, nvm dirs, `.venv`/`site-packages` lists, macOS-only unit files) ships the home machine's assumptions to every deployment. The enumeration never closes (asdf, mise, Docker, Windows, Nix...).

How to apply: Before hardcoding an environmental fact, find the decider that already generalizes: capture the installing shell's env at install time instead of enumerating toolchain dirs; let the repo's git index decide source-vs-generated instead of listing vendor dir names; make degradation loud and self-describing instead of quietly compensating for one host's misconfiguration. If a fix mentions a literal path from this machine, it is probably the wrong layer. Related: [[journey-first-design]], the net-simplification rule.
