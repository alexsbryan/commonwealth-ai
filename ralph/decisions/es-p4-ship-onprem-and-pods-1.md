<!-- ledger -->

**es-p4-ship-onprem-and-pods-1 · 2026-10-23 · es-p4-ship-onprem-and-pods · worker** — this commit
- Needed: whether the row was still owed its outcome after d18d2ee22 closed it
- Chose: no; the row has no deliverable left, so the session closes it and commits no code
- Because: the staged target/debug once carried HOME's absolute path (targets are moved to PROMOTE_HOME before landing), so a missing target/debug/llama-server reads "this lane never built", not "the tree lost it"; and no kernel is visible in the container, so the daemon cannot boot there. The remaining image check is manifest-vs-PIN.toml, which d18d2ee22's `[build]` table shows is already in the image

<!-- appendix -->

## es-p4-ship-onprem-and-pods-1 · 2026-10-23 — es-p4 was done at d18d2ee22; the row owes nothing and the image's remaining verification is the manifest match, already in the image

<details><summary>reasoning, evidence, package</summary>

**The row is marked done and landed on the queue branch.** `ralph/next/engine-swap/STATE.md:54`
reads `- [x] es-p4-ship-onprem-and-pods d18d2ee22`, and `origin/eutropia/engine-swap`
(`3a85af9c2`) contains its lane (`git merge-base --is-ancestor` on `origin/landed/es-portable`
holding `5ed8251b7`). The session was handed a row that is already closed; the lane worktree
is at `5ed8251b7` with a clean tree.

The row's three deliverables (`git show --stat d18d2ee22`):

- the on-prem kit's loop names `llama-server ggml-rpc-server`
  (`distributions/deploy/onprem/install.sh:186`), plus the manifest at `:192-193`;
  `package.sh:125` stages both binaries into `$KIT/bin` via `build.sh --install-dir`;
- the ROCm `Containerfile:199-215,265-266` swaps the backend to `hip` and runs `build.sh
  --backend rocm --install-dir /usr/local/bin`, copying the two binaries and the manifest
  into the runtime stage;
- the `Containerfile.cuda` sibling runs the same `swap-inference-backend.py cuda`.

**The image, inspected on this host** (`flatpak-spawn --host podman run ... 00c08bb964d0`,
`ghcr.io/alexsbryan/sovereign-rocm:es-p4`, built 2026-10-24T01:16Z):

- `/usr/local/bin` holds `llama-server`, `ggml-rpc-server`, `llama-server.manifest.toml`;
- `ldd /usr/local/bin/llama-server` resolves `libamdhip64.so.7`, `libhipblas.so.3`,
  `librocblas.so.5` under `/opt/rocm-7.2.0/lib`;
- `strings -a llama-server | grep -c gfx942` = 270 and a `gfx942` token survives;
- the manifest's `[build]` table says `backend = "rocm"`.

**Why the daemon was not booted in the image, and the corrected premise.** The lane's first
attempt to stand the shipped server on a lane-local path failed because `target/debug/` holds no
`llama-server`: staged targets are moved to `PROMOTE_HOME` before landing (engine-swap's
preparation), so that absence says the lane never built the server, not that the tree lost it.
Keeping the check inside the image is the honest alternative, but `podman run` here reports
`no ROCm-capable device is detected` and `Available devices: (none)`, so no model can load and the
daemon cannot reach `/health` — the user in the image is root and no kernel device is present.
The one shippable check left inside the container inference-free is the manifest match:
`manifest_matches_pin.py` reproduces `llama_router::build_pin::verify`'s comparison (commit,
llguidance, binary names, every patch digest; `[build]` excluded, as `verify` excludes it).
It PASSES on the image's manifest against the kit's `PIN.toml`, and REFUSES on a one-character
commit change — the plant the row's "finds a verified server" bar names.

</details>
