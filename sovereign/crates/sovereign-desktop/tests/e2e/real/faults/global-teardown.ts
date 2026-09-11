// SPDX-License-Identifier: AGPL-3.0-or-later
// THE HARNESS KILLS WHAT ITS SUBJECT STARTED.
//
// The app under test spawns a daemon and deliberately does NOT own its
// lifecycle: the supervisor's child and, since svt-1, anything
// `ServingHost` brings up are spawned DETACHED (`process_group(0)`), so
// the node survives the window closing. That is the product behaviour the
// sv-surface campaign exists to protect — it is not a leak, and the fix
// for this suite is NOT to teach the desktop to kill a daemon again.
//
// It does leave the harness owing a reap, and until 2026-09-11 this file
// paid it in a way that only worked on Linux: it swept `pgrep -f 'daemon
// run'` and discriminated by reading `/proc/<pid>/environ`. macOS has no
// /proc, the read threw, the catch swallowed it, and the surviving daemon
// showed up one run later as global-setup's ":9741 is occupied" — a
// failure that names the wrong thing and blames the wrong run.
//
// Three reaps now, cheapest and most precise first:
//   1. the desktop instance global-setup spawned (its pid file),
//   2. every daemon that wrote a pidfile under a scratch HOME this run
//      baked — the daemon's OWN record of itself, not a guess, and the
//      same file `svrn daemon stop` keys off,
//   3. a port backstop for a daemon that died before writing one or was
//      started outside a profile.
//
// The backstop reaps ONLY the ports global-setup wrote into
// `faults-owned-ports.json` after proving each one free. That file is the
// claim, and without it this step does nothing: a teardown that ran with no
// setup behind it — a crashed setup, a stray `playwright test --grep` — has
// no standing to kill whatever is on :9741, and on a dev box that is the
// operator's own daemon. The ports are not the harness's by convention;
// they are the harness's because it took them and said so.
//
// A claimed port still occupied after all three is REPORTED, not swallowed.
// The next run would fail on it anyway, one run late and pointing at nothing.
import { execSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { ARTIFACTS, portInUse } from "./spawn";

const PID_FILE = path.join(ARTIFACTS, "faults-app.pid");

/** Written by global-setup once it has proved each port free. */
const OWNED_PORTS_FILE = path.join(ARTIFACTS, "faults-owned-ports.json");

/** The ports THIS run claimed, or none. See the header. */
function ownedPorts(): number[] {
  try {
    const claimed = JSON.parse(fs.readFileSync(OWNED_PORTS_FILE, "utf8"));
    return Array.isArray(claimed) ? claimed.filter((n) => Number.isFinite(n)) : [];
  } catch {
    return [];
  }
}

/** Where a daemon writes its pidfile under a scratch HOME. Both spellings:
 *  `rebrand::svrnmesh_root()` is legacy-aware and resolves `~/.sovereign`
 *  when that is what the profile baked (spawn.ts writes config there). */
const ROOT_NAMES = [".svrnmesh", ".sovereign"];

async function killAndWait(pid: number, what: string): Promise<void> {
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    return; // already gone
  }
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    try {
      process.kill(pid, 0);
    } catch {
      return;
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  try {
    process.kill(pid, "SIGKILL");
    console.log(`[faults-teardown] SIGKILL ${what} (pid ${pid}) — SIGTERM was not enough`);
  } catch {
    /* gone between the check and the signal */
  }
}

/** Every `<profile>/home/<root>/daemon.pid` this run could have produced. */
function daemonPidFiles(): string[] {
  let entries: string[] = [];
  try {
    entries = fs.readdirSync(ARTIFACTS);
  } catch {
    return [];
  }
  const found: string[] = [];
  for (const entry of entries) {
    if (!entry.includes("profile")) continue;
    for (const root of ROOT_NAMES) {
      const candidate = path.join(ARTIFACTS, entry, "home", root, "daemon.pid");
      if (fs.existsSync(candidate)) found.push(candidate);
    }
  }
  return found;
}

/** PIDs LISTENING on `port`, via lsof (present on both macOS and Linux). */
function listenersOn(port: number): number[] {
  try {
    return execSync(`lsof -ti tcp:${port} -sTCP:LISTEN || true`, { shell: "/bin/bash" })
      .toString()
      .split("\n")
      .map((s) => Number(s.trim()))
      .filter((n) => Number.isFinite(n) && n > 1);
  } catch {
    return [];
  }
}

export default async function globalTeardown(): Promise<void> {
  // 1. The desktop global-setup spawned.
  if (fs.existsSync(PID_FILE)) {
    const pid = Number(fs.readFileSync(PID_FILE, "utf8").trim());
    fs.rmSync(PID_FILE, { force: true });
    if (Number.isFinite(pid) && pid > 1) await killAndWait(pid, "faults desktop");
  }

  // 2. Every daemon that recorded itself under a scratch HOME.
  for (const file of daemonPidFiles()) {
    const pid = Number(fs.readFileSync(file, "utf8").trim());
    if (Number.isFinite(pid) && pid > 1) {
      await killAndWait(pid, `daemon from ${path.relative(ARTIFACTS, file)}`);
    }
    fs.rmSync(file, { force: true });
  }

  // 3. The backstop, over the ports this run actually claimed.
  const claimed = ownedPorts();
  if (claimed.length === 0) {
    console.log(
      "[faults-teardown] no faults-owned-ports.json — global-setup did not claim any port " +
        "this run, so nothing is reaped by port. Anything listening belongs to someone else.",
    );
    return;
  }
  for (const port of claimed) {
    for (const pid of listenersOn(port)) {
      console.log(`[faults-teardown] :${port} still held by pid ${pid} — reaping`);
      await killAndWait(pid, `listener on :${port}`);
    }
  }
  fs.rmSync(OWNED_PORTS_FILE, { force: true });

  const leaked: number[] = [];
  for (const port of claimed) {
    if (await portInUse(port)) leaked.push(port);
  }
  if (leaked.length > 0) {
    throw new Error(
      `faults teardown: ${leaked.map((p) => `:${p}`).join(", ")} still occupied after the ` +
        `reap. Something this run started is not reachable by pidfile or by lsof; the next ` +
        `run would fail in global-setup instead, one run late.`,
    );
  }
}
