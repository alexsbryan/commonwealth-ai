# RELEASE DRIVERS: THREE DEFECTS THAT COULD SHIP THE WRONG BYTES — FIXED + GATED (2026-08-08, commit 0deb4912, found cutting cli-v0.5.0).

RELEASE DRIVERS: THREE DEFECTS THAT COULD SHIP THE WRONG BYTES — FIXED + GATED (2026-08-08, commit `0deb4912`, found cutting cli-v0.5.0).

1. STALE ARTIFACTS UNDER A NEW TAG (the dangerous one).
`release-cli-local.sh` uploaded `dist/sovereign-*.tar.gz` by GLOB and `dist/` is never cleaned.
NOTHING downstream could catch a leftover: the filename is built from `$VERSION` so it proves
nothing, the `.sha256` is regenerated FROM the stale bytes so it verifies cleanly, and the
`SHA256SUMS` step deliberately preserves assets it did not rebuild — i.e. it actively blesses them.
cli-v0.5.0 was ONE successful build leg from publishing Jul-29 (v0.4.x) binaries for 2 of 3 targets;
only an unrelated crash before the upload step stopped it. THE DESKTOP ALREADY LIVED THIS:
desktop-v0.3.5 (2026-07-27) shipped a byte-identical 0.3.3 payload under a 0.3.5 name, correctly
signed — the .sig signs BYTES, and those bytes were a genuine 0.3.3 build — and users who updated
landed back on 0.3.3 with the prompt never clearing. Desktop got a payload-version gate then; the
CLI never did.
FIX: `package()` writes `dist/sovereign-<triple>.tar.gz.buildinfo` (version + commit) at the only
moment the contents are known; the upload path refuses any tarball whose sidecar disagrees with the
release being cut. Four verdicts, not two: shippable / stale-version / stale-commit / unverifiable.

2. PODMAN PREFLIGHT SKIPPED FOR THE LEG THAT NEEDED IT. `release-all.sh` checked podman under
`if ! (( SKIP_DESKTOP ))`, but the CLI's x86_64-unknown-linux-gnu leg builds in the SAME container.
`--skip-desktop` skipped the preflight the CLI depended on → stopped VM surfaced 9 MINUTES in, after
both mac legs were compiled and packaged. FIX: gate on the WORK (`!SKIP_DESKTOP || !SKIP_CLI`), and
prove reachability with `podman info` up front.

3. UNREACHABLE PODMAN REPORTED AS MISSING IMAGE. `podman image exists` ran BEFORE
`podman machine start`, so a stopped VM printed "container image missing — run
build-desktop-linux.sh" and sent you to rebuild a 3.3GB image already present. FIX: start → prove
reachable → only then judge the image. Both messages now name the other cause to rule it out.

TESTS: `scripts/tests/` (7 cases, ~2s, `gh`+`podman` stubbed, mktemp dir), run by
`scripts/pre-push.sh` whenever `^scripts/release-.*\.sh$|^scripts/tests/` changes — same conditional
shape as the CLI journey self-test. The provenance suite asserts a CLEAN build is still ACCEPTED, so
the gate is proven to discriminate rather than merely always-refuse.

CORRECTION OF A PLAUSIBLE-BUT-WRONG DIAGNOSIS I published mid-session: I claimed the pre-push hook
"gates only the files a push touches" and that this let unformatted code reach main. WRONG —
`fmt_gate` already runs `cargo fmt --all --check` (whole workspace); it is only CONDITIONED on the
push touching Rust paths. Both live worktrees have `core.hooksPath=.githooks`, so nothing was
bypassed. The real effect is WHO PAYS: commits land on local main between pushes, so the gate fires
for whoever pushes next, on someone else's code — cli-v0.5.0 absorbed three sweeps (34, 1, 1 files),
none its own. Mitigation: `pre-commit.sh` now rustfmt-checks STAGED .rs files and says so,
WARN-ONLY / exit 0 always (the 2026-08-06 operator directive keeps that hook advisory). The push
gate remains the blocker.
