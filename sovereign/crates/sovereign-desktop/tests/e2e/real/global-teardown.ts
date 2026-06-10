// SPDX-License-Identifier: AGPL-3.0-or-later
// Stops the real sovereign-desktop process started by global-setup.ts.
// SIGTERM first (clean daemon-child + store shutdown), SIGKILL after a
// 10s grace. The scratch profile is left on disk for inspection — the
// next run's setup wipes it (unless SOVEREIGN_REAL_KEEP_PROFILE=1).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PID_FILE = path.resolve(__dirname, "../../../test-results/real-app.pid");

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

export default async function globalTeardown(): Promise<void> {
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
