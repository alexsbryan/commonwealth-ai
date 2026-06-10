// SPDX-License-Identifier: AGPL-3.0-or-later
// Boot-time faults. Each test spawns its OWN desktop instance on a
// dedicated bridge port (the shared supervised instance on :9745 keeps
// running untouched) and asserts the documented degradation contract —
// the app must always reach a deliberate state, never hang or die.
import { expect, test } from "@playwright/test";
import path from "node:path";
import { ARTIFACTS, awaitSticky, spawnDesktop } from "./spawn";

test("missing model at startup routes to setup-required (no hang, no crash)", async () => {
  test.setTimeout(180_000);
  const app = await spawnDesktop({
    profileDir: path.join(ARTIFACTS, "faults-missing-model"),
    bridgePort: 9747,
    logName: "faults-missing-model.log",
    profile: {
      chatModel: "/nonexistent/model-that-was-deleted.gguf",
      // desktop.toml-only profile: the missing-model guard lives on
      // the DesktopLegacy boot path (main.rs setup task).
      cliSetupConfig: false,
    },
  });
  try {
    // The contract: model_path missing → setup_complete cleared →
    // `setup-required` emitted. The bridge's sticky replay proves it
    // without any UI.
    await awaitSticky(app.bridge, "setup-required", 120_000);
  } finally {
    await app.stop();
  }
});

test("supervised child that crash-loops falls back without wedging the app", async () => {
  test.setTimeout(240_000);
  // A "daemon" that dies instantly, every time: the supervisor must
  // give up (crash-loop ceiling / startup deadline) and the desktop
  // must still come up via its documented fallback (in-process
  // daemon), reaching backend-ready rather than hanging.
  const app = await spawnDesktop({
    profileDir: path.join(ARTIFACTS, "faults-crashloop"),
    bridgePort: 9748,
    logName: "faults-crashloop.log",
    supervisor: true,
    cliPath: "/usr/bin/false",
  });
  try {
    const ready = await awaitSticky(app.bridge, "backend-ready", 200_000);
    expect(ready !== undefined).toBe(true);
  } finally {
    await app.stop();
  }
});
