# Start the daemon

Everything in svrnmesh runs through one background process: the daemon. It
loads the models, serves the API on `localhost:9741`, watches your corpora
and code indexes, and speaks to the mesh. Every other guide in these docs
assumes you have it — this page is the one place that gets you there.

You have this prerequisite when `svrn doctor` comes back clean. If it
already does, go back to whatever sent you here.

## Install the CLI

Prebuilt binaries for macOS (Apple Silicon) and Linux (x86_64):

```sh
curl -fsSL https://svrnme.sh/install.sh | sh
```

That drops the CLI into `~/.local/bin`. You'll want 8 GB of RAM to start —
16 is comfortable, 32 runs the best open models.

Already installed? `svrn update --check` says whether a newer release
exists; `svrn update` installs it in place, through the same
checksum-verified installer.

### Or build from source

Needs a Rust toolchain and CMake. On macOS: `xcode-select --install`, then
`export SDKROOT="$(xcrun --show-sdk-path)"` (bindgen needs the system
headers). On Linux: `sudo apt install cmake build-essential
protobuf-compiler`.

```sh
cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
ln -sf "$(pwd)/target/release/sovereign-cli" ~/.local/bin/svrn
```

AMD Strix Halo or a cloud-GPU peer take a little more — see the
[toolbox](../sovereign/docs/TOOLBOX_SETUP.md) and
[cloud-peer](../sovereign/docs/CLOUD_PEER_DEPLOY.md) guides.

## First run

```sh
svrn setup
```

Setup detects your hardware, downloads models that fit it, writes the
config, and starts the daemon. Add `--yes` to accept the recommended
choices non-interactively, `--data-dir <path>` to keep state somewhere
other than the default.

Two facts about what it leaves behind:

- State lives under `~/.svrnmesh/` (`~/.sovereign` is the legacy name for
  the same directory): `config.toml`, models, indexes, logs. The client
  API is `localhost:9741`; the mesh-internal port is `:9742`. If `:9741`
  is taken, set `client_port` under `[daemon]` in `config.toml`. (The
  [architecture tour](./ARCHITECTURE_TOUR.md) has the full port table.)
- The first daemon boot quietly founds a **solo mesh** — a private mesh of
  one machine. That matters the day you [join a mesh](./JOIN_A_MESH.md);
  until then you'll never notice it.

## Verify you have it

```sh
svrn doctor                          # the health check — run this first, always
curl http://localhost:9741/v1/models # the API answering, models loaded
```

`doctor` diagnoses rather than reports: when something is wrong it names
the repair command.

## Keep it running

Setup registers the daemon with launchd (macOS) or systemd (Linux) where
it can; if it told you to run `svrn install-service`, do — that
registration is what restarts the daemon after a crash and brings it back
across logouts. A daemon started by hand with `svrn daemon start` is
unsupervised until you install the service.

Day-to-day lifecycle: `svrn daemon status / start / stop / restart`, and
`svrn daemon reload` to apply config changes without a restart. The
[runbook](../sovereign/docs/RUNBOOK.md) is the operator-grade detail —
pidfiles, log rotation, readiness timeouts, and what supervision does and
doesn't restart.

## Undo

`svrn daemon stop` stops it; `svrn uninstall-service` removes the
registration; `svrn setup --reset` wipes the config and starts over
(uninstalling the service first).

## When it breaks

`svrn doctor`, then the [troubleshooting guide](../sovereign/docs/TROUBLESHOOTING.md)
— symptom-to-fix pairs for the maintainer. Helping someone on the desktop
app instead? [Having trouble?](../sovereign/docs/HAVING_TROUBLE.md) covers
the same ground without a terminal.
