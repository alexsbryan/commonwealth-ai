// SPDX-License-Identifier: AGPL-3.0-or-later
// B7 — the Pi writes the code.
//
// A RAW beat by physics: the machine in frame is a Raspberry Pi across
// the room, and no browser automation reaches it. What it is NOT is
// unverified. Every other beat in this reel refuses to export unless its
// claim was proven in the same run; a hand-recorded clip that skipped
// that would be the one place the reel takes a promise on trust — and it
// would be the most impressive clip in the set, which is exactly where a
// viewer's scepticism goes.
//
// So the footage is human, and the claim is machine-checked against the
// agent-coding battery's own report:
//
//   sovereign agent-bench run --problems 3.2 --report \
//     sovereign/crates/sovereign-desktop/test-artifacts/demo/raw/b7-pi-coding.bench.json
//
// The gate reads that report and asserts the run actually solved the
// problem — held-out fixtures green, clean exit, and a score at or above
// the floor. `3.2-lights-out` (GF(2) over the light grid) is the problem
// on purpose: it is the hardest tier the battery ships, so a passing
// receipt is worth showing.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { rawBeatTest, expect } from "./beat";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");

/** The problem the take is built around. */
const PROBLEM = process.env.SOVEREIGN_DEMO_BENCH_PROBLEM ?? "3.2-lights-out";
/** Total (0..=9) the run must reach. Correctness is asserted separately
 *  and absolutely — this floor is about approach + efficiency, i.e. "it
 *  didn't just brute-force its way to a green test run". */
const FLOOR = Number(process.env.SOVEREIGN_DEMO_BENCH_FLOOR ?? 7);
/** A receipt from three months ago does not describe the build you are
 *  filming. */
const MAX_AGE_DAYS = Number(process.env.SOVEREIGN_DEMO_BENCH_MAX_AGE_DAYS ?? 14);
const REPORT =
  process.env.SOVEREIGN_DEMO_BENCH_REPORT ??
  path.join(CRATE_ROOT, "test-artifacts/demo/raw/b7-pi-coding.bench.json");

interface DimensionScore {
  raw: number;
  source?: { kind?: string };
}
interface ProblemScore {
  problem_id: string;
  dim_a: DimensionScore;
  dim_b: DimensionScore;
  dim_c: DimensionScore;
  total: number;
  exit_reason?: { kind?: string };
  is_partial?: boolean;
  wall_ms?: number;
  witness_summary?: {
    verify_exit_ok: boolean;
    passed: number;
    failed: number;
    total: number;
  } | null;
}
interface BenchReport {
  agent: string;
  model: string;
  judge_model?: string;
  finished_at?: string;
  per_problem: ProblemScore[];
  grand_total?: number;
  max_total?: number;
}

rawBeatTest(
  {
    id: "b7-pi-coding",
    capture: "raw",
    title: "A Raspberry Pi solves a problem it has never seen",
    claim:
      "The model on the Pi doesn't autocomplete — it reads a spec, writes a program, " +
      "and the held-out tests it never saw pass.",
    gifPadSec: 1.2,
    recordingGuide: [
      `Run the battery on the Pi: \`sovereign agent-bench run --problems ${PROBLEM.split("-")[0]} --report ${path.relative(CRATE_ROOT, REPORT)}\`.`,
      "Record the Pi's screen (or the terminal driving it) at 1280×800, or 16:10 at any size.",
      "Beats: the problem statement · the agent working · the held-out tests running green · " +
        "the final score.",
      "The gate below re-reads the report you just wrote — shoot the run that produced it, " +
        "not a different one.",
    ],
    script: [
      { text: "A $80 computer. No cloud.", holdMs: 3000 },
      { text: "It has never seen this problem.", holdMs: 3000 },
      { text: "Held-out tests. Green.", holdMs: 3200 },
    ],
  },
  async ({ run }) => {
    run.requireOrSkip(
      fs.existsSync(REPORT),
      `no bench report at ${REPORT}. Produce one on the Pi with ` +
        `\`sovereign agent-bench run --problems ${PROBLEM.split("-")[0]} --report <path>\` ` +
        `(or point SOVEREIGN_DEMO_BENCH_REPORT at an existing one). Without it there is no ` +
        `receipt for what this clip claims, and the exporter will not encode the take.`,
    );

    let report: BenchReport;
    try {
      report = JSON.parse(fs.readFileSync(REPORT, "utf8")) as BenchReport;
    } catch (e) {
      throw new Error(
        `bench report at ${REPORT} is not readable JSON: ${e instanceof Error ? e.message : e}`,
      );
    }

    // ── the report is about the build being filmed ──
    if (report.finished_at) {
      const ageDays = (Date.now() - Date.parse(report.finished_at)) / 86_400_000;
      run.requireOrSkip(
        Number.isFinite(ageDays) && ageDays <= MAX_AGE_DAYS,
        `the bench report finished ${ageDays.toFixed(1)}d ago (${report.finished_at}), past the ` +
          `${MAX_AGE_DAYS}d window. Re-run the battery so the receipt describes the build you ` +
          `are filming.`,
      );
    }

    const score = (report.per_problem ?? []).find(
      (p) => p.problem_id === PROBLEM || p.problem_id.startsWith(`${PROBLEM}-`),
    );
    run.requireOrSkip(
      !!score,
      `the report covers [${(report.per_problem ?? []).map((p) => p.problem_id).join(", ") || "nothing"}] ` +
        `but not \`${PROBLEM}\`.`,
    );

    // ── it finished on its own terms ──
    const exitKind = score!.exit_reason?.kind ?? "completed";
    expect(
      exitKind,
      `the agent must have exited cleanly — a timeout or token-budget kill is a partial run, ` +
        `and a partial run is not the claim this clip makes`,
    ).toBe("completed");
    expect(score!.is_partial ?? false, "the run must not be scored as partial").toBe(false);

    // ── the held-out tests, which are the whole point ──
    const w = score!.witness_summary;
    expect(
      w,
      `no witness summary — correctness on \`${PROBLEM}\` is auto-scored from the held-out ` +
        `fixtures, and without them there is nothing objective behind the clip`,
    ).toBeTruthy();
    expect(w!.verify_exit_ok, "the held-out verify step must exit clean").toBe(true);
    expect(w!.failed, "no held-out test may fail").toBe(0);
    expect(w!.passed, "at least one held-out test must have run and passed").toBeGreaterThan(0);
    run.mark("witness-green");

    // ── correctness is absolute; the rest has a floor ──
    expect(
      score!.dim_a.raw,
      "correctness (dim_a) is auto-scored from the held-out fixtures and must be a full 3 — " +
        "this clip's claim is that the program WORKS, not that it nearly worked",
    ).toBe(3);
    expect(
      score!.total,
      `total ${score!.total}/9 is below the ${FLOOR}/9 floor. Correctness alone can pass while ` +
        `approach (dim_b, GF(2) vs brute force) and efficiency (dim_c) are weak — the clip ` +
        `implies a good solution, not merely a passing one. Lower the floor with ` +
        `SOVEREIGN_DEMO_BENCH_FLOOR if you mean to film a weaker run.`,
    ).toBeGreaterThanOrEqual(FLOOR);
    run.mark("score-floor-met");

    run.note(
      `bench receipt: ${score!.problem_id} — ${score!.total}/9 ` +
        `(correctness ${score!.dim_a.raw}, approach ${score!.dim_b.raw}, efficiency ${score!.dim_c.raw}) · ` +
        `held-out ${w!.passed}/${w!.total} passed · ` +
        `agent ${report.agent} · model ${report.model}` +
        (score!.wall_ms ? ` · ${(score!.wall_ms / 1000).toFixed(0)}s wall` : "") +
        ` · ${path.relative(CRATE_ROOT, REPORT)}`,
    );
  },
);
