// SPDX-License-Identifier: AGPL-3.0-or-later
// Extracts the authoritative Tauri command manifest from the literal
// `tauri::generate_handler![...]` block in src-tauri/src/main.rs.
//
// The block is hand-maintained Rust, so this is a regex parse with a
// loud drift guard: if the extracted count ever drops below MIN_COMMANDS
// (or the block can't be found), the script throws rather than silently
// reporting shrunken coverage.
//
// Usable both as a CLI (`node command-manifest.mjs`) and as a module
// (`import { extractManifest } from ...` — used by coverage-report.mjs).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const MAIN_RS = path.resolve(__dirname, "../../../src-tauri/src/main.rs");
const MIN_COMMANDS = 150;

/** @returns {{ commands: Array<{name: string, module: string}>, total: number }} */
export function extractManifest(mainRsPath = MAIN_RS) {
  const src = fs.readFileSync(mainRsPath, "utf8");
  const start = src.indexOf("tauri::generate_handler![");
  if (start === -1) {
    throw new Error(
      `command-manifest: no generate_handler! block found in ${mainRsPath}`,
    );
  }
  const open = src.indexOf("[", start);
  const close = src.indexOf("]", open);
  if (close === -1) {
    throw new Error("command-manifest: unterminated generate_handler! block");
  }
  const block = src.slice(open + 1, close);

  const commands = [];
  for (const rawLine of block.split("\n")) {
    const line = rawLine.replace(/\/\/.*$/, "").trim().replace(/,$/, "");
    if (!line) continue;
    // Entries look like `commands::send_message` or `mesh_commands::mesh_join`.
    const m = line.match(/^([A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*)$/);
    if (!m) {
      throw new Error(
        `command-manifest: unparseable entry in generate_handler!: ${JSON.stringify(rawLine)}`,
      );
    }
    const segments = m[1].split("::");
    commands.push({
      name: segments[segments.length - 1],
      module: segments.length > 1 ? segments.slice(0, -1).join("::") : "(root)",
    });
  }

  if (commands.length < MIN_COMMANDS) {
    throw new Error(
      `command-manifest: extracted only ${commands.length} commands ` +
        `(< ${MIN_COMMANDS}); the generate_handler! parse has drifted — fix me`,
    );
  }
  const names = new Set(commands.map((c) => c.name));
  if (names.size !== commands.length) {
    const dupes = commands
      .map((c) => c.name)
      .filter((n, i, arr) => arr.indexOf(n) !== i);
    throw new Error(
      `command-manifest: duplicate command names: ${[...new Set(dupes)].join(", ")}`,
    );
  }
  return { commands, total: commands.length };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const manifest = extractManifest();
  console.log(JSON.stringify(manifest, null, 2));
  console.error(`(${manifest.total} commands)`);
}
