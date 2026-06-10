// SPDX-License-Identifier: AGPL-3.0-or-later
// Stop the supervised desktop (its SIGTERM also reaps the daemon
// child) plus any stray child daemons left by killed instances.
import { execSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { ARTIFACTS } from "./spawn";

const PID_FILE = path.join(ARTIFACTS, "faults-app.pid");

export default async function globalTeardown(): Promise<void> {
  if (fs.existsSync(PID_FILE)) {
    const pid = Number(fs.readFileSync(PID_FILE, "utf8").trim());
    fs.rmSync(PID_FILE, { force: true });
    if (Number.isFinite(pid) && pid > 1) {
      try {
        process.kill(pid, "SIGTERM");
        await new Promise((r) => setTimeout(r, 5000));
        process.kill(pid, "SIGKILL");
      } catch {
        /* already gone */
      }
    }
  }
  // Sweep surviving daemon children — but ONLY ours. The cmdline
  // (`… daemon run`) is indistinguishable from a dev daemon, so
  // discriminate by the HOME env baked into the scratch profile.
  try {
    const pids = execSync("pgrep -f 'daemon run' || true", { shell: "/bin/bash" })
      .toString()
      .split("\n")
      .filter(Boolean);
    for (const pid of pids) {
      try {
        const environ = fs.readFileSync(`/proc/${pid}/environ`, "utf8");
        if (environ.includes("faults-profile")) {
          process.kill(Number(pid), "SIGKILL");
        }
      } catch {
        /* gone or unreadable */
      }
    }
  } catch {
    /* none */
  }
}
