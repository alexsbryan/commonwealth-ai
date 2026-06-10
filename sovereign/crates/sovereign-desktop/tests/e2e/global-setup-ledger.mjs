// SPDX-License-Identifier: AGPL-3.0-or-later
// Truncates the per-run coverage ledger so coverage-report.mjs always
// reflects exactly one suite run (parallel workers append to it during
// the run; see fixtures/test-base.ts).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const LEDGER_PATH = path.resolve(
  __dirname,
  "../../test-artifacts/ledger-synthetic.jsonl",
);

export default function globalSetup() {
  fs.rmSync(LEDGER_PATH, { force: true });
}
