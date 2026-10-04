// SPDX-License-Identifier: AGPL-3.0-or-later
// Stops the real sovereign-desktop process started by global-setup.ts.
// SIGTERM first (clean daemon-child + store shutdown), SIGKILL after a
// 10s grace. The scratch profile is left on disk for inspection — the
// next run's setup wipes it (unless SOVEREIGN_REAL_KEEP_PROFILE=1).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const PID_FILE = path.join(CRATE_ROOT, "test-artifacts/real-app.pid");
const DAEMON_PID_FILE = path.join(CRATE_ROOT, "test-artifacts/real-daemon.pid");

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

export default async function globalTeardown(): Promise<void> {
  // Stop the harness-owned daemon first (it holds the shared index dir
  // open). By PID, never `daemon stop`: with a launchd/systemd service
  // registered that command delegates to the service manager and stops
  // the OPERATOR's daemon, not ours — the same trap that made `daemon
  // start` drive the production daemon on 2026-07-30 (global-setup's
  // startManagedDaemon spawns `daemon run` and banks the PID for this).
  // Attach mode (the opt-out) never started a daemon; no PID file, no-op.
  if (fs.existsSync(DAEMON_PID_FILE)) {
    const dpid = Number(fs.readFileSync(DAEMON_PID_FILE, "utf8").trim());
    fs.rmSync(DAEMON_PID_FILE, { force: true });
    if (Number.isFinite(dpid) && dpid > 1 && alive(dpid)) {
      try {
        process.kill(dpid, "SIGTERM");
      } catch {
        /* already gone */
      }
      const deadline = Date.now() + 15_000;
      while (Date.now() < deadline && alive(dpid)) {
        await new Promise((r) => setTimeout(r, 250));
      }
      if (alive(dpid)) {
        console.warn(`[real-teardown] managed daemon pid ${dpid} ignored SIGTERM — SIGKILL`);
        try {
          process.kill(dpid, "SIGKILL");
        } catch {
          /* already gone */
        }
      }
    }
  }

  if (!fs.existsSync(PID_FILE)) return;
  const pid = Number(fs.readFileSync(PID_FILE, "utf8").trim());
  fs.rmSync(PID_FILE, { force: true });
  if (!Number.isFinite(pid) || pid <= 1 || !alive(pid)) return;

  try {
    process.kill(pid, "SIGTERM");
  } catch {
    return;
  }
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline && alive(pid)) {
    await new Promise((r) => setTimeout(r, 250));
  }
  if (alive(pid)) {
    console.warn(`[real-teardown] pid ${pid} ignored SIGTERM — SIGKILL`);
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      /* already gone */
    }
  }
}
