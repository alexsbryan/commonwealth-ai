#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// SABOTAGE — does this suite actually notice when the product breaks?
//
// # The gap this closes
//
// Everything else we measure describes what the suite REACHED: spec counts,
// invoke-coverage (36/251 commands), the fixture-liveness gate. None of it
// describes what the suite would CATCH. Those are different properties, and
// only the second one is the reason tests exist. A spec that navigates to a
// page, waits for a selector, and asserts nothing meaningful contributes to
// every coverage number we have and defends nothing.
//
// So: break the product on purpose, run the specs that claim to cover it, and
// require them to go red. A mutant the suite does not kill is a hole with a
// name and a file:line — the most actionable defect report the test suite can
// produce about itself.
//
//   CAUGHT   — the declared specs failed. That invariant is genuinely defended.
//   SURVIVED — the product was broken and the suite stayed green. A user would
//              hit this in production and no gate would have stopped it.
//   STALE    — the mutation no longer applies (the code moved). The bank is
//              lying about what it covers; fix the mutant, not this script.
//
// # Why textual mutation of real source
//
// Not a mocked failure or an injected fault: the actual `src/` file is edited,
// so what runs is a real regression in a real build. A fault flag would only
// prove the suite notices the flag. The cost is that this script writes to
// tracked files — see SAFETY below, which is the load-bearing part of it.
//
// # Usage
//
//   node tests/e2e/scripts/sabotage.mjs             # whole bank (synthetic)
//   node tests/e2e/scripts/sabotage.mjs --list      # print the bank, run nothing
//   node tests/e2e/scripts/sabotage.mjs --only <id>
//   node tests/e2e/scripts/sabotage.mjs --suite real
//   node tests/e2e/scripts/sabotage.mjs --json out.json
//   node tests/e2e/scripts/sabotage.mjs --allow-dirty   # see SAFETY
//   node tests/e2e/scripts/sabotage.mjs --bank ./exploratory-bank.mjs
//
// Exit 0 only when every selected mutant was CAUGHT.
// Exit 1 on any SURVIVED or STALE. Exit 2 on a safety failure (see below).
//
// # SAFETY
//
// This script edits files git is tracking. Four rules, none optional:
//   1. It refuses to start if any target file has uncommitted changes
//      (`--allow-dirty` to override) — a crash mid-run must never be able to
//      destroy work that was not committed, which git could not give back.
//   2. It takes an exclusive lock. Two concurrent runs is silent corruption,
//      not a clash: the second captures the first's MUTATED file as its
//      "original" and restores a deliberate bug into the tree permanently.
//   3. Originals are held in memory AND written to test-artifacts/.sabotage/
//      before the first edit.
//   4. Restoration runs from a `finally`, and again from handlers on SIGINT,
//      SIGTERM and uncaughtException; afterwards every file is compared
//      BYTE-FOR-BYTE against what was read. Not `git diff` — that calls a
//      legitimately-dirty target unrestored and a restored one clean only by
//      luck. On any mismatch it exits 2, never 0 with the tree modified.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const BACKUP_DIR = path.join(CRATE_ROOT, "test-artifacts", ".sabotage");

const CONFIG_FOR_SUITE = {
  synthetic: "playwright.config.ts",
  real: "playwright.real.config.ts",
};

// ── args ──
const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const value = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};

const ONLY = value("--only", null);
const SUITE = value("--suite", "synthetic");
const JSON_OUT = value("--json", null);
// An alternate bank. The committed one is a regression floor; an exploratory
// bank (e.g. source-first mutants aimed at finding holes rather than
// confirming coverage) can be run without touching it.
const BANK_PATH = value("--bank", path.join(__dirname, "../sabotage-bank.mjs"));

const { BANK } = await import(new URL(`file://${path.resolve(BANK_PATH)}`).href);

if (!CONFIG_FOR_SUITE[SUITE]) {
  console.error(`unknown --suite ${SUITE} (expected: ${Object.keys(CONFIG_FOR_SUITE).join(", ")})`);
  process.exit(2);
}

const selected = BANK.filter((m) => (ONLY ? m.id === ONLY : m.suite === SUITE));
if (ONLY && selected.length === 0) {
  console.error(`no mutant with id ${JSON.stringify(ONLY)}. --list to see the bank.`);
  process.exit(2);
}

if (flag("--list")) {
  for (const m of BANK) {
    console.log(`${m.id}  [${m.suite}]`);
    console.log(`    breaks : ${m.breaks}`);
    console.log(`    target : ${m.target}`);
    console.log(`    caught : ${m.mustFail.join(", ")}`);
    console.log(`    user   : ${m.userImpact}`);
  }
  process.exit(0);
}

// ── safety: originals, held and restored unconditionally ──
/** @type {Map<string, string>} absolute path → original content */
const originals = new Map();
let restored = false;

// Declared up here, not where it is acquired, so the signal handlers below can
// release it even if they fire during preflight.
const LOCK = path.join(BACKUP_DIR, "lock");
let holdsLock = false;

/** The dev server this run owns, if it started one. Declared here so the
 *  signal handlers below can take it down whenever they fire. */
let devServer = null;

function releaseLock() {
  if (!holdsLock) return;
  holdsLock = false;
  try {
    fs.unlinkSync(LOCK);
  } catch {
    /* already gone */
  }
}

function restoreAll() {
  if (restored) return;
  restored = true;
  for (const [abs, content] of originals) {
    try {
      fs.writeFileSync(abs, content);
    } catch (e) {
      console.error(`\nCANNOT RESTORE ${abs}: ${e.message}`);
    }
  }
  releaseLock();
}

/** Verify against the bytes we captured, NOT against git. `git diff --quiet`
 *  would call a legitimately-dirty target "unrestored" and a legitimately
 *  restored one "clean" only by luck; comparing to the original content is
 *  exact, and works whether or not the file had uncommitted changes. */
function verifyRestored() {
  const bad = [];
  for (const [abs, content] of originals) {
    let now;
    try {
      now = fs.readFileSync(abs, "utf8");
    } catch {
      bad.push(abs);
      continue;
    }
    if (now !== content) bad.push(abs);
  }
  return bad;
}

function git(args) {
  return spawnSync("git", args, { cwd: CRATE_ROOT, encoding: "utf8" });
}

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    console.error(`\n[sabotage] ${sig} — restoring sources before exit`);
    restoreAll();
    stopSharedServer();
    process.exit(130);
  });
}
process.on("uncaughtException", (e) => {
  console.error(`\n[sabotage] uncaught: ${e.stack ?? e}`);
  restoreAll();
  stopSharedServer();
  process.exit(2);
});

// ── preflight ──
const targets = [...new Set(selected.map((m) => path.resolve(CRATE_ROOT, m.target)))];

for (const abs of targets) {
  if (!fs.existsSync(abs)) {
    console.error(`STALE BANK: target does not exist: ${path.relative(CRATE_ROOT, abs)}`);
    process.exit(1);
  }
}

const dirty = git(["status", "--porcelain", "--", ...targets.map((t) => path.relative(CRATE_ROOT, t))]);
if (dirty.stdout.trim() && !flag("--allow-dirty")) {
  console.error(
    "REFUSING TO RUN — these target files have uncommitted changes:\n" +
      dirty.stdout.trimEnd() +
      "\n\nThis script rewrites tracked source and restores it afterwards. If it " +
      "were SIGKILLed mid-run against a dirty file, the uncommitted version is " +
      "what you would lose — git could not give it back.\n" +
      "Commit or stash first, or pass --allow-dirty if you accept that risk " +
      `(originals are copied to ${path.relative(CRATE_ROOT, BACKUP_DIR)}/ before ` +
      "the first edit either way).",
  );
  process.exit(2);
}
if (dirty.stdout.trim()) {
  console.log("[sabotage] --allow-dirty: mutating files with uncommitted changes\n");
}

fs.mkdirSync(BACKUP_DIR, { recursive: true });

// Concurrency lock. Two runs at once is silent corruption, not a clash: the
// second one reads the first one's MUTATED file as its "original" and restores
// the mutation permanently. Nothing downstream would look wrong — the tree
// would just quietly carry a deliberate bug.
try {
  fs.writeFileSync(LOCK, String(process.pid), { flag: "wx" });
  holdsLock = true;
} catch {
  const owner = Number(fs.readFileSync(LOCK, "utf8").trim());
  let alive = false;
  try {
    process.kill(owner, 0); // signal 0 = liveness probe, sends nothing
    alive = true;
  } catch {
    /* gone */
  }
  if (alive) {
    console.error(
      `REFUSING TO RUN — another sabotage run (pid ${owner}) holds the lock.\n` +
        "Two runs would each capture the other's mutated file as its original " +
        "and restore a deliberate bug into the tree. Wait for it to finish.",
    );
    process.exit(2);
  }
  console.log(`[sabotage] clearing stale lock from dead pid ${owner}`);
  fs.writeFileSync(LOCK, String(process.pid));
  holdsLock = true;
}

for (const abs of targets) {
  const content = fs.readFileSync(abs, "utf8");
  originals.set(abs, content);
  fs.writeFileSync(path.join(BACKUP_DIR, path.basename(abs) + ".orig"), content);
}

// ── the dev server ──
//
// A bank run is ~26 Playwright invocations. Under CI the config sets
// `reuseExistingServer: false`, so each one cold-starts its own Vite — which
// measured as the majority of the wall clock, spent restarting a server whose
// state none of the mutants depend on. So own the lifecycle here: start one,
// tell the config it may reuse it (SABOTAGE_SHARED_SERVER), stop it at the end.
//
// Correctness rests on Vite's watcher invalidating the module graph when a
// mutant lands, which is the same path every local run already takes
// (`reuseExistingServer` is true off-CI) and is why mutants are given a settle
// beat before the specs run.
const DEV_URL = "http://localhost:5173";

async function serverUp() {
  try {
    await fetch(DEV_URL, { signal: AbortSignal.timeout(2000) });
    return true;
  } catch {
    return false;
  }
}

async function startSharedServer() {
  if (await serverUp()) {
    console.log("[sabotage] reusing the dev server already on :5173");
    process.env.SABOTAGE_SHARED_SERVER = "1";
    return;
  }
  console.log("[sabotage] starting one dev server for the whole run");
  devServer = spawn("npm", ["run", "dev"], {
    cwd: CRATE_ROOT,
    stdio: "ignore",
    detached: true, // its own process group, so we can take the whole tree down
  });
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    if (await serverUp()) {
      process.env.SABOTAGE_SHARED_SERVER = "1";
      return;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error("dev server never came up on :5173");
}

function stopSharedServer() {
  if (!devServer?.pid) return;
  try {
    process.kill(-devServer.pid, "SIGTERM"); // negative pid = the group
  } catch {
    /* already gone */
  }
  devServer = null;
}

// ── running specs ──
function runSpecs(specs, suite) {
  const config = CONFIG_FOR_SUITE[suite];
  const r = spawnSync(
    "npx",
    ["playwright", "test", "-c", config, ...specs, "--reporter=line"],
    { cwd: CRATE_ROOT, encoding: "utf8", env: process.env },
  );
  const out = `${r.stdout ?? ""}${r.stderr ?? ""}`;
  const num = (re) => Number((out.match(re) ?? [])[1] ?? 0);
  return {
    code: r.status ?? -1,
    out,
    passed: num(/(\d+) passed/),
    failed: num(/(\d+) failed/),
  };
}

/** Vite serves from disk but its watcher is async; give it a beat to
 *  invalidate the module graph before Playwright navigates. */
const settle = (ms) => new Promise((r) => setTimeout(r, ms));

// ── the run ──
const results = [];
let exitCode = 0;

/** Sentinel: a red baseline is a clean abort, not a crash. */
class BaselineRed extends Error {}

// The window this opens is real and it has already bitten once: on 2026-07-28 a
// commit landed WHILE a run was mid-mutation and captured `ChatView.svelte`
// with a deliberate bug in it (HEAD got the mutant; the tree got the correct
// file back seconds later, so it showed up as a backwards-looking diff). The
// lock above stops two sabotage runs from colliding; nothing here can stop
// `git commit`. Say so where it cannot be missed.
console.log(
  `\n!! ${targets.length} TRACKED FILE(S) WILL BE REWRITTEN IN PLACE FOR THE ` +
    `NEXT FEW MINUTES.\n` +
    `!! Do not commit, stash, or switch branches until this finishes — a commit\n` +
    `!! taken mid-run captures a deliberate bug. Files:\n` +
    targets.map((t) => `!!   ${path.relative(CRATE_ROOT, t)}`).join("\n") +
    "\n",
);

try {
  await startSharedServer();

  // Baseline. A mutant "caught" by a spec that was ALREADY failing proves
  // nothing at all — the same positive-control rule the invariant bank runs
  // under. One baseline per distinct spec set, since that is the expensive part.
  const specSets = new Map();
  for (const m of selected) {
    const key = m.mustFail.slice().sort().join("|");
    if (!specSets.has(key)) specSets.set(key, m.mustFail);
  }

  console.log(`[sabotage] baseline: ${specSets.size} spec set(s) must be GREEN before mutating\n`);
  for (const specs of specSets.values()) {
    const { code, out } = runSpecs(specs, SUITE);
    if (code !== 0) {
      console.error(
        `BASELINE RED for [${specs.join(", ")}] — aborting.\n` +
          `Every mutant against these specs would report CAUGHT for a reason ` +
          `that has nothing to do with the mutation. Fix the suite first.\n\n` +
          out.split("\n").slice(-25).join("\n"),
      );
      exitCode = 2;
      // Unwind to the `finally` so sources are restored and verified, without
      // a stack trace printed over the diagnostic above.
      throw new BaselineRed();
    }
    console.log(`  ✓ baseline green: ${specs.join(", ")}`);
  }

  console.log(`\n[sabotage] ${selected.length} mutant(s)\n`);

  for (const m of selected) {
    const abs = path.resolve(CRATE_ROOT, m.target);
    const original = originals.get(abs);

    // Anti-staleness: the mutation must land in exactly one place. Zero means
    // the code moved and this mutant has been testing nothing; more than one
    // means the blast radius is wider than declared and a CAUGHT verdict would
    // not be attributable to the invariant named.
    const hits = original.split(m.find).length - 1;
    if (hits !== 1) {
      console.log(`  STALE    ${m.id}`);
      console.log(
        `           find-string occurs ${hits}× in ${m.target} (need exactly 1)\n` +
          `           ${JSON.stringify(m.find)}`,
      );
      results.push({ ...m, verdict: "STALE", hits });
      exitCode = 1;
      continue;
    }

    fs.writeFileSync(abs, original.replace(m.find, m.replace));
    await settle(1500);
    let verdict, r;
    try {
      r = runSpecs(m.mustFail, m.suite);
      verdict = r.code !== 0 ? "CAUGHT" : "SURVIVED";
    } finally {
      fs.writeFileSync(abs, original);
    }

    const wanted = m.expectVerdict ?? "CAUGHT";
    const ok = verdict === wanted;

    if (m.selfControl) {
      // The runner's own negative control. See the bank entry.
      console.log(`  ${ok ? "CONTROL✓" : "CONTROL✗"} ${m.id}`);
      if (!ok) {
        console.log(
          `           This mutation cannot affect ${m.mustFail.join(", ")}, so a ` +
            `CAUGHT verdict means this script reports CAUGHT for reasons other ` +
            `than the mutation. EVERY OTHER VERDICT IN THIS RUN IS UNTRUSTWORTHY.`,
        );
        exitCode = 1;
      }
    } else if (m.knownHole) {
      // A gap we have measured and not yet closed. Tracked rather than
      // blocking: failing CI on a hole we already knew about on the day we
      // found it just teaches people to delete the entry. What this DOES buy
      // is the other direction — when someone finally covers the behaviour,
      // the entry flips and says so, instead of sitting here claiming a gap
      // that closed months ago.
      if (ok) {
        console.log(`  HOLE     ${m.id}`);
        console.log(`           still uncovered: ${m.userImpact}`);
      } else {
        console.log(`  HOLE FIXED  ${m.id}`);
        console.log(
          `           Something now catches this. Promote it: drop knownHole + ` +
            `expectVerdict and set mustFail to the spec that caught it, so it ` +
            `becomes a real regression gate instead of a stale complaint.`,
        );
      }
    } else if (verdict === "CAUGHT") {
      // A mutation that fails EVERY test in the spec is usually a crash, not a
      // caught regression — it proves the page loaded, not that any assertion
      // watches this behaviour. Surgical kills leave siblings green.
      //
      // Sometimes a whole spec file legitimately hangs off one behaviour. That
      // is allowed, but it has to be STATED (`bluntKill`), never inferred from
      // finding it — the same rule the fixture-liveness gate runs under. An
      // undeclared blunt kill warns; a declaration that stops being true also
      // warns, so the claim cannot quietly rot into a rubber stamp.
      const blunt = r.passed === 0 && r.failed > 0;
      const declared = typeof m.bluntKill === "string";
      console.log(
        `  CAUGHT   ${m.id}` +
          (blunt && !declared ? "   (blunt: killed every test in the spec)" : "") +
          (declared && !blunt ? "   (stale bluntKill declaration)" : ""),
      );
      if (blunt && !declared) {
        console.log(
          `           No test in ${m.mustFail.join(", ")} survived. Confirm the ` +
            `mutation is not simply crashing the page — a crash is caught by any ` +
            `spec at all, which is not what this mutant claims. If the whole file ` +
            `genuinely depends on this behaviour, say so in \`bluntKill\`.`,
        );
      }
      if (declared && !blunt) {
        console.log(
          `           bluntKill says every test depends on this, but ${r.passed} ` +
            `passed. The claim is out of date — drop it, or narrow the mutant.`,
        );
      }
    } else {
      console.log(`  SURVIVED ${m.id}`);
      console.log(`           broke: ${m.breaks}`);
      console.log(`           user would see: ${m.userImpact}`);
      console.log(`           these specs stayed GREEN: ${m.mustFail.join(", ")}`);
      exitCode = 1;
    }
    results.push({
      ...m,
      verdict,
      expected: wanted,
      passed: r.passed,
      failed: r.failed,
      tail: (r.out ?? "").split("\n").slice(-6).join("\n"),
    });
    await settle(500);
  }
} catch (e) {
  if (!(e instanceof BaselineRed)) throw e;
} finally {
  restoreAll();
  stopSharedServer();
  const unrestored = verifyRestored();
  if (unrestored.length > 0) {
    console.error(
      "\nSAFETY FAILURE: these sources do not match what we read before mutating:\n" +
        unrestored.map((f) => `  ${path.relative(CRATE_ROOT, f)}`).join("\n") +
        `\n\nByte-exact originals are in ${path.relative(CRATE_ROOT, BACKUP_DIR)}/ ` +
        "(one <basename>.orig per file). Restore from those — they are the only " +
        "copy of any uncommitted state, which `git checkout` would not return.",
    );
    process.exit(2);
  }
}

// ── report ──
if (exitCode === 2) process.exit(2); // baseline red: nothing below is meaningful

// Three populations, scored apart. Folding them together would let a passing
// self-control pad the ratio it exists to validate, and let known holes drag
// down a number that is supposed to describe the regression gate.
const controls = results.filter((r) => r.selfControl);
const holes = results.filter((r) => r.knownHole);
const mutants = results.filter((r) => !r.selfControl && !r.knownHole);
const caught = mutants.filter((r) => r.verdict === "CAUGHT").length;
const survived = mutants.filter((r) => r.verdict === "SURVIVED");
const stale = results.filter((r) => r.verdict === "STALE");
const brokenControls = controls.filter((r) => r.verdict !== "SURVIVED");
const closedHoles = holes.filter((r) => r.verdict === "CAUGHT");

console.log(`\n${"─".repeat(64)}`);
if (brokenControls.length > 0) {
  console.log(
    `SELF-CONTROL FAILED — this script reported CAUGHT for a mutation that ` +
      `cannot affect the specs it ran. Treat the ${caught}/${mutants.length} ` +
      `below as unverified.`,
  );
}
console.log(`sabotage: ${caught}/${mutants.length} mutants caught`);
if (controls.length > 0) {
  console.log(
    `          ${controls.length - brokenControls.length}/${controls.length} self-control(s) held ` +
      `(the runner can still report SURVIVED)`,
  );
}
if (holes.length > 0) {
  console.log(
    `          ${holes.length - closedHoles.length} known hole(s) still open` +
      (closedHoles.length ? `, ${closedHoles.length} newly closed — promote them` : ""),
  );
  for (const h of holes) {
    console.log(`            ${h.verdict === "CAUGHT" ? "closed" : "open"}  ${h.id}`);
  }
}
if (survived.length) {
  console.log(`\n${survived.length} SURVIVED — the suite does not defend these:`);
  for (const s of survived) console.log(`  · ${s.id} — ${s.breaks}`);
  console.log(
    `\nEach line is a regression that would reach a user with every gate green.\n` +
      `Fix by strengthening the named spec, not by deleting the mutant.`,
  );
}
if (stale.length) {
  console.log(`\n${stale.length} STALE — the bank no longer matches the code:`);
  for (const s of stale) console.log(`  · ${s.id} → ${s.target}`);
}

if (JSON_OUT) {
  fs.writeFileSync(
    path.resolve(JSON_OUT),
    JSON.stringify({ suite: SUITE, caught, total: results.length, results }, null, 2),
  );
  console.log(`\nwrote ${JSON_OUT}`);
}

process.exit(exitCode);
