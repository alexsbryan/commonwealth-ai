# A

Calling a `#[cfg(unix)]` function (e.g. `find_daemon_pid_by_port`, which shells
to lsof/ss) from code that is NOT itself `#[cfg(unix)]`-gated compiles fine on
the dev Mac and passes the full `sovereign-test.sh` workspace gate — because the
host target is unix. It fails ONLY on the Windows (`not(unix)`) leg of the
desktop release build with `error[E0425]: cannot find function ... in this
scope`. The desktop leg cross-compiles all targets; Windows builds LAST (in the
podman/xwin container, `/work/...` paths), so the failure can surface ~hours into
`release-all.sh`.

Hit on 2026-07-22 releasing 0.3.2: `sovereign-cli-daemon/.../lifecycle.rs:344`
called the unix-only fn un-gated in `start_daemon()`'s bind-collision detector.
Fix = add `#[cfg(unix)]` on the call block, mirroring the identical guard already
at line ~60. Committed as `964dafe5 fix win error`.

Why: the release test gate only proves the host target; it cannot catch
Windows-only cfg mismatches, and there is no cheap local Windows check
(`sovereign-cli-daemon` pulls `sovereign-inference` → `llama-cpp-sys`, whose
build script needs the cross toolchain the dev box lacks). The in-container
desktop build is the only verifier.

How to apply: when adding or calling any `#[cfg(unix)]`/`#[cfg(target_os)]`
helper, confirm EVERY call site is under a matching cfg. To re-run only the
failed desktop leg after a fix: `./scripts/release-all.sh --skip-cli
--skip-tests --force` (`--force` needed once the tag is already published;
`--skip-tests` is safe because a cfg-gated change is byte-identical on the unix
host that the gate already tested). Related release-leg trap:
[[invariant_cli_linux_leg_needs_taskset_glslc_guard]].
