#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Generate quality/conformance/desktop.toml from `@<REQ-ID>` tags on the
// Playwright specs.
//
// The Rust side does this with a syn-based scanner over `/// covers:` doc
// comments; this is the same idea for TypeScript, and it deliberately emits
// the IDENTICAL `[[claim]]` shape so `svrn conformance` needs no third route —
// it already joins quality/conformance/*.toml against a JUnit report, and
// Playwright now writes one.
//
// Tag a spec with Playwright's native tag support:
//
//   test("the epistemic footer renders the verdict receipt",
//        { tag: ["@GR-44"] }, async ({ page }) => { ... });
//
// `test` is what the claim binds to, in `<classname>::<name>` form matching
// the JUnit the reporter writes. A tag naming an id the registry does not
// carry fails the check rather than being counted.
//
//   node tests/e2e/scripts/conformance-tags.mjs          # verify, exit 1 if stale
//   UPDATE_CONFORMANCE_TAGS=1 node .../conformance-tags.mjs   # rewrite
import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { join, relative } from "node:path";

const CRATE = new URL("../../..", import.meta.url).pathname.replace(/\/$/, "");
const ROOT = new URL("../../../../../..", import.meta.url).pathname.replace(/\/$/, "");
const SPECS = join(CRATE, "tests/e2e/specs");
const OUT = join(ROOT, "quality/conformance/desktop.toml");
const REGISTRY = join(ROOT, "quality/requirements.toml");

// Registry ids, read straight out of the generated TOML — no TOML parser
// needed for a file whose ids are one-per-line.
const known = new Set(
  readFileSync(REGISTRY, "utf8")
    .split("\n")
    .filter((l) => l.startsWith("id = "))
    .map((l) => l.slice(6, -1)),
);

const claims = [];
const unknown = [];
for (const f of readdirSync(SPECS).filter((f) => f.endsWith(".spec.ts"))) {
  const text = readFileSync(join(SPECS, f), "utf8");
  const lines = text.split("\n");
  // The JUnit reporter names a case `<describe> › <title>` and puts the FILE in
  // classname. The claim key has to match that exactly or it resolves to
  // nothing — which renders as never-ran, indistinguishable from a requirement
  // nobody claimed. That is the highest-risk silent failure in this join, so
  // the describe title is carried rather than assumed absent.
  let describe = "";
  lines.forEach((line, i) => {
    const d = line.match(/^\s*test\.describe\(\s*(["'`])(.+?)\1/);
    if (d) describe = d[2];
    // test("title", { tag: ["@GR-44", "@GR-47"] }, async ...)
    const m = line.match(/^\s*test\(\s*(["'`])(.+?)\1\s*,\s*\{[^}]*tag:\s*\[([^\]]*)\]/);
    if (!m) return;
    const title = m[2];
    const ids = [...m[3].matchAll(/["'`]@([A-Z][A-Z-]*-\d+)["'`]/g)].map((x) => x[1]);
    for (const id of ids) {
      if (!known.has(id)) { unknown.push(`${f}:${i + 1} @${id}`); continue; }
      const name = describe ? `${describe} \u203a ${title}` : title;
      claims.push({ requirement: id, file: relative(ROOT, join(SPECS, f)), line: i + 1, test: `${f}::${name}` });
    }
  });
}

if (unknown.length) {
  console.error(`conformance-tags: ${unknown.length} tag(s) name no requirement:\n  ${unknown.join("\n  ")}`);
  process.exit(1);
}

claims.sort((a, b) => a.requirement.localeCompare(b.requirement) || a.test.localeCompare(b.test));
const body = `# Conformance tags in the desktop app — GENERATED. DO NOT EDIT BY HAND.
#
#   UPDATE_CONFORMANCE_TAGS=1 node tests/e2e/scripts/conformance-tags.mjs
#
# Each claim maps a requirement id from research/clean-room/REQUIREMENTS.md to
# the Playwright spec that proves it. \`test\` is \`<file>::<title>\`, matching the
# JUnit the playwright reporter writes to test-results/junit.xml, so
# \`svrn conformance\` joins a claim to a real per-test verdict without guessing.
#
# These are INTERACTIVE specs against the running app — the instrument the
# desktop-class and chat-surface requirements actually need.

${claims.map((c) => `[[claim]]
requirement = "${c.requirement}"
test = "${c.test}"
file = "${c.file}"
line = ${c.line}
asserts = 1
`).join("\n")}`;

if (process.env.UPDATE_CONFORMANCE_TAGS === "1") {
  writeFileSync(OUT, body);
  console.log(`conformance-tags: wrote ${claims.length} claim(s) to ${relative(ROOT, OUT)}`);
} else if (!existsSync(OUT) || readFileSync(OUT, "utf8") !== body) {
  console.error(`conformance-tags: ${relative(ROOT, OUT)} is stale.\nRegenerate:\n  UPDATE_CONFORMANCE_TAGS=1 node tests/e2e/scripts/conformance-tags.mjs`);
  process.exit(1);
} else {
  console.log(`conformance-tags: ${claims.length} claim(s), up to date`);
}
