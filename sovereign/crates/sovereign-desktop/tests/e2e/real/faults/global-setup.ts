// SPDX-License-Identifier: AGPL-3.0-or-later
// Global setup for the fault-injection suite: one SUPERVISED desktop
// (SOVEREIGN_USE_SUPERVISOR=1 → spawns `sovereign-cli daemon run` as a
// child under the scratch HOME and attaches to it). The kill-midstream
// and cancel-storm specs share this instance; crash-loop and
// missing-model specs spawn their own short-lived instances on other
// bridge ports via spawn.ts.
//
// Hard requirements:
// - :9741 must be FREE — the supervised child owns it. Stop the dev
//   daemon first (`sovereign daemon stop`).
// - :9745 must be free (no other harness desktop running).
import { execSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

import {
  ARTIFACTS,
  awaitSticky,
  CRATE_ROOT,
  portInUse,
  REPO_ROOT,
  spawnDesktop,
} from "./spawn";

const PID_FILE = path.join(ARTIFACTS, "faults-app.pid");

export default async function globalSetup(): Promise<void> {
  fs.mkdirSync(ARTIFACTS, { recursive: true });

  // :9741 must be free for the BOOT-FAULT instances (their bootstrap
  // probes it; an occupant flips them to Attach and skips the paths
  // under test). The shared supervised child runs on :9751 instead.
  for (const [port, why] of [
    [9741, "stop the dev daemon (`sovereign daemon stop`)"],
    [9745, "another harness desktop is running — stop it"],
    [9751, "a previous faults child daemon survived — kill it"],
  ] as const) {
    if (await portInUse(port)) {
      throw new Error(`faults setup: :${port} is occupied — ${why}.`);
    }
  }

  if (!fs.existsSync(path.join(CRATE_ROOT, "dist/index.html"))) {
    execSync("npm run build", { cwd: CRATE_ROOT, stdio: "inherit" });
  }
  execSync("cargo build -p sovereign-desktop -p sovereign-cli -p sovereign-cli-daemon", {
    cwd: REPO_ROOT,
    stdio: "inherit",
    timeout: 15 * 60 * 1000,
  });

  console.log("[faults-setup] spawning SUPERVISED desktop (child daemon on :9741)");
  const app = await spawnDesktop({
    profileDir: path.join(ARTIFACTS, "faults-profile"),
    bridgePort: 9745,
    logName: "faults-app.log",
    supervisor: true,
    profile: { daemonPort: 9751 },
  });
  fs.writeFileSync(PID_FILE, String(app.pid));

  // Boot budget covers child-daemon spawn + health probe + model wiring.
  await awaitSticky(app.bridge, "backend-ready", 240_000);
  const sup = await awaitSticky(app.bridge, "supervisor-state", 30_000);
  console.log(`[faults-setup] backend-ready ✓ supervisor-state=${JSON.stringify(sup)}`);
  const label = JSON.stringify(sup);
  if (!label.toLowerCase().includes('"kind":"healthy"')) {
    throw new Error(
      `faults setup: supervisor is not Healthy (${label}) — the kill/recovery ` +
        `specs need a supervised child daemon. See ${app.logPath}`,
    );
  }
}
