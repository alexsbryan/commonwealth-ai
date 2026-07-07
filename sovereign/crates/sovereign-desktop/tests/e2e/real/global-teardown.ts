// SPDX-License-Identifier: AGPL-3.0-or-later
// Stops the real sovereign-desktop process started by global-setup.ts.
// SIGTERM first (clean daemon-child + store shutdown), SIGKILL after a
// 10s grace. The scratch profile is left on disk for inspection — the
// next run's setup wipes it (unless SOVEREIGN_REAL_KEEP_PROFILE=1).
import { execSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const REPO_ROOT = path.resolve(CRATE_ROOT, "../../..");
const PID_FILE = path.join(CRATE_ROOT, "test-artifacts/real-app.pid");
const HOME = path.join(CRATE_ROOT, "test-artifacts/real-profile/home");
const DAEMON_BIN = path.join(REPO_ROOT, "target/debug/sovereign-cli-daemon");

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

export default async function globalTeardown(): Promise<void> {
  // Stop the harness-owned daemon first (it holds the shared index dir open).
  if (process.env.SOVEREIGN_REAL_MANAGED_DAEMON === "1") {
    try {
      execSync(`${JSON.stringify(DAEMON_BIN)} daemon stop`, {
        env: {
          ...process.env,
          HOME,
          XDG_CONFIG_HOME: path.join(HOME, ".config"),
          XDG_DATA_HOME: path.join(HOME, ".local/share"),
          XDG_CACHE_HOME: path.join(HOME, ".cache"),
        },
        stdio: "inherit",
        timeout: 30_000,
      });
    } catch {
      console.warn("[real-teardown] managed-daemon: `daemon stop` failed (may already be down)");
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
