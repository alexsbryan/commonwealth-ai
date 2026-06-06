#!/usr/bin/env node
// Aggregate every bundle's meshapp.json into public/meshapp/catalog.json — the
// build-time index the host UI loads to discover available apps (instead of a
// hard-coded list). Registry-ready: each meshapp.json is the self-describing
// unit; this is just their index. Installed third-party apps will merge in via
// a host-side scan in a later phase. Run from pre{dev,build}.
import { readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "public", "meshapp");
const out = join(root, "catalog.json");

const apps = [];
for (const entry of readdirSync(root, { withFileTypes: true })) {
  // Skip non-app dirs (the shared `_sdk/`) and anything without a manifest.
  if (!entry.isDirectory() || entry.name.startsWith("_")) continue;
  const manifest = join(root, entry.name, "meshapp.json");
  if (!existsSync(manifest)) continue;
  const m = JSON.parse(readFileSync(manifest, "utf8"));
  if (m.id !== entry.name) {
    throw new Error(`meshapp.json id "${m.id}" != directory "${entry.name}"`);
  }
  apps.push(m);
}
apps.sort((a, b) => a.id.localeCompare(b.id));
writeFileSync(out, JSON.stringify(apps, null, 2) + "\n");
console.log(`meshapp catalog: ${apps.length} apps → ${out}`);
