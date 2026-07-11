// SPDX-License-Identifier: AGPL-3.0-or-later
// Persona-QA metric computation — ONE implementation shared by the
// scoreboard (per-run rows) and the gates (pass/fail against
// persona-gates.toml). Journal rows are reclassified under the current
// taxonomy so every evaluation is on the same ruler.
import { reclassifyRow, GAP_FAMILY } from "./classify.mjs";

export function quantile(xs, p) {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(p * s.length))];
}

/// rows = parsed journal lines (possibly from MULTIPLE runs, concatenated —
/// bigger N). Returns raw numbers; formatting is the caller's job.
export function computeMetrics(rows) {
  const turns = rows
    .filter((r) => r.kind === "turn")
    .map((r) => ({ ...r, outcome: reclassifyRow(r) }));
  const sessions = rows.filter((r) => r.kind === "session_end");

  const satisfied = sessions.filter((s) => s.endReason === "satisfied").length;
  const abandoned = sessions.filter((s) => s.endReason === "abandoned").length;
  const grounded = turns.filter((t) => t.outcome === "answered_grounded").length;
  const rescued = turns.filter((t) => t.outcome === "rescued_by_web").length;
  const hallucinations = turns.filter((t) => t.aligned?.verdict === "hallucination").length;
  const flips = turns.filter((t) => t.flip?.flipped).length;
  const cancels = turns.filter((t) => t.outcome === "canceled_slow").length;
  const silentGaps = turns.filter((t) => t.outcome === "silent_gap").length;
  const selfIndicted = turns.filter((t) =>
    (t.answer ?? "").includes("which does not appear in the sources"),
  ).length;
  const postured = turns.filter((t) => t.posture);
  const ttfts = turns.map((t) => t.ttftMs).filter((x) => x != null);

  // TTV per session: cumulative latency up to the first turn whose answer
  // the user-judge accepted (rescues count via the refined judge).
  const ttvs = [];
  const bySession = new Map();
  for (const t of turns) {
    if (!bySession.has(t.session)) bySession.set(t.session, []);
    bySession.get(t.session).push(t);
  }
  for (const st of bySession.values()) {
    let acc = 0;
    for (const t of st.sort((a, b) => a.turn - b.turn)) {
      acc += t.latencyMs ?? 0;
      const good = t.judge && !t.judge.broken && t.judge.score < 6;
      if (t.outcome === "rescued_by_web" || (good && t.outcome === "answered_grounded")) {
        ttvs.push(acc);
        break;
      }
    }
  }
  const rephrases = sessions.reduce((a, s) => a + (s.rephrases ?? 0), 0);

  return {
    nSessions: sessions.length,
    nTurns: turns.length,
    gfr: sessions.length ? satisfied / sessions.length : null,
    abandon_rate: sessions.length ? abandoned / sessions.length : null,
    grounded_rate: turns.length ? grounded / turns.length : null,
    rescued,
    hallucinations,
    flips,
    cancels,
    silent_gap_rate: turns.length ? silentGaps / turns.length : null,
    self_indictment_rate: turns.length ? selfIndicted / turns.length : null,
    ttft_p50_s: ttfts.length ? quantile(ttfts, 0.5) / 1000 : null,
    ttft_p95_s: ttfts.length ? quantile(ttfts, 0.95) / 1000 : null,
    ttdraft_p50_s: (() => {
      const xs = turns.map((t) => t.ttdraftMs).filter((x) => x != null);
      return xs.length ? quantile(xs, 0.5) / 1000 : null;
    })(),
    ttv_median_s: ttvs.length ? quantile(ttvs, 0.5) / 1000 : null,
    ttv_sessions_with_value: ttvs.length,
    grace_mean: postured.length
      ? postured.reduce((a, t) => a + t.posture.score, 0) / postured.length
      : null,
    rephrases_per_session: sessions.length ? rephrases / sessions.length : null,
    turns,
    sessions,
  };
}

export { GAP_FAMILY };
