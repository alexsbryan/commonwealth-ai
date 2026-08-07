// Glassbox: OutputChannel + ring buffer of the last 20 suggestions
// (extension plan §glassbox). Every daemon response carries
// sovereign_debug — what fed the suggestion, what rule stopped it,
// how long it took. "Explain Last Suggestion" renders the most
// recent record; "Diagnose Completion Setup" runs the sequenced
// probes and prints PASS/FAIL plus the copy-pasteable fix for the
// first failure.

import * as vscode from "vscode";
import {
  completeFim,
  probeStatus,
  servesFim,
  servesNextEdit,
  SovereignDebug,
} from "./client";

export interface SuggestionRecord {
  when: Date;
  path: string;
  language: string;
  text: string;
  finishReason: string;
  wallMs: number;
  debug: SovereignDebug | null;
}

const RING_SIZE = 20;

export class FimDebugLog implements vscode.Disposable {
  readonly channel = vscode.window.createOutputChannel("svrn fim");
  private ring: SuggestionRecord[] = [];

  record(r: SuggestionRecord): void {
    this.ring.push(r);
    if (this.ring.length > RING_SIZE) this.ring.shift();
    const d = r.debug;
    this.channel.appendLine(
      `[${r.when.toISOString()}] ${r.path} — ${r.text.length} chars, ` +
        `${r.wallMs}ms wall, finish=${r.finishReason}` +
        (d
          ? `, model=${d.model_id ?? "?"}, slot=${d.slot ?? "?"}, mode=${d.mode ?? "?"}, ` +
            `stop=${d.stop_rule ?? "?"}, ttft=${d.timings_ms?.ttft ?? "?"}ms, ` +
            `total=${d.timings_ms?.total ?? "?"}ms`
          : ""),
    );
  }

  noteError(msg: string): void {
    this.channel.appendLine(`[${new Date().toISOString()}] ERROR ${msg}`);
  }

  explainLast(): void {
    const last = this.ring[this.ring.length - 1];
    if (!last) {
      this.channel.appendLine("no suggestions recorded yet in this session");
    } else {
      this.channel.appendLine("── last suggestion ─────────────────────");
      this.channel.appendLine(`when:         ${last.when.toISOString()}`);
      this.channel.appendLine(`file:         ${last.path} (${last.language})`);
      this.channel.appendLine(`latency:      ${last.wallMs}ms (client-measured)`);
      this.channel.appendLine(`finish:       ${last.finishReason}`);
      if (last.debug) {
        const d = last.debug;
        this.channel.appendLine(`model:        ${d.model_id ?? "?"}`);
        this.channel.appendLine(`slot:         ${d.slot ?? "?"}`);
        // Present on every /v1/completions record — that route only
        // serves when the slot HAS a FIM lane. Absence means the daemon
        // predates the field, not that the model lacked markers.
        this.channel.appendLine(`fim_style:    ${d.fim_style ?? "(not reported)"}`);
        this.channel.appendLine(`mode:         ${d.mode ?? "?"}`);
        this.channel.appendLine(`stop_rule:    ${d.stop_rule ?? "?"}`);
        this.channel.appendLine(`prompt_chars: ${d.prompt_chars ?? "?"}`);
        this.channel.appendLine(`emitted:      ${d.emitted_chars ?? "?"} chars`);
        this.channel.appendLine(
          `timings:      ttft=${d.timings_ms?.ttft ?? "?"}ms total=${d.timings_ms?.total ?? "?"}ms`,
        );
      } else {
        this.channel.appendLine("(no sovereign_debug payload on this one)");
      }
      this.channel.appendLine("── text ────────────────────────────────");
      this.channel.appendLine(last.text);
    }
    this.channel.show(true);
  }

  /** Sequenced probes — stops at the first FAIL with the fix. */
  async diagnose(endpoint: string): Promise<void> {
    const out = this.channel;
    out.clear();
    out.appendLine("svrn fim — setup diagnostic");
    out.appendLine(`endpoint: ${endpoint}`);
    out.appendLine("");

    // 1. Daemon up?
    const s = await probeStatus(endpoint, 4000);
    if (!s.daemonUp) {
      out.appendLine("FAIL  daemon unreachable at /status");
      out.appendLine("");
      out.appendLine("Fix: start the daemon — `sovereign daemon run` — then re-run this.");
      out.show(true);
      return;
    }
    out.appendLine("PASS  daemon reachable (/status)");

    // 2. Editing model available at all?
    const e = s.edit;
    if (!e) {
      out.appendLine("FAIL  no editing model available (inference.edit absent)");
      out.appendLine("");
      out.appendLine("Fix: add to ~/.svrnmesh/config.toml:");
      out.appendLine("");
      out.appendLine("  [models.edit]");
      out.appendLine('  path = "/path/to/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"');
      out.appendLine("");
      out.appendLine("then `sovereign daemon restart`. Any competent chat model can serve");
      out.appendLine("next-edit suggestions; ghost text (FIM) additionally needs a coder");
      out.appendLine("model whose vocab carries FIM markers — the daemon's boot log says so");
      out.appendLine("explicitly if the vocab probe found none.");
      out.show(true);
      return;
    }

    // 2b. Which lanes does it actually serve? Each is present exactly
    //     when the slot can serve it — never inferred from the model id.
    const fimLane = servesFim(e);
    const nextEditLane = servesNextEdit(e);
    out.appendLine(
      `PASS  edit slot live: ${e.model_id} on slot '${e.slot}'` +
        (e.aliased_to_fast ? " (shared fast slot — lean mode)" : ""),
    );
    out.appendLine(
      `      next edit:               ${nextEditLane ? e.next_edit_format : "unavailable"}`,
    );
    out.appendLine(
      `      inline completion (FIM): ${fimLane ? e.fim_style : "unavailable"}`,
    );
    if (e.degraded) {
      out.appendLine(
        "      provenance:              resident chat model (no [models.edit] set)",
      );
    }
    // The daemon composes the one operator-facing next step. Render it
    // verbatim — four surfaces read this field, and each inventing its
    // own wording is how they end up disagreeing.
    if (e.advice) {
      out.appendLine("");
      out.appendLine(`NOTE  ${e.advice}`);
    }

    if (!nextEditLane && !fimLane) {
      out.appendLine("");
      out.appendLine("FAIL  the editing model serves neither lane");
      out.appendLine("");
      out.appendLine("Fix: point [models.edit].path at a chat or coder GGUF and restart");
      out.appendLine("the daemon; the boot log's [edit] lines name what it rejected.");
      out.show(true);
      return;
    }

    // 3. No FIM lane is a SUPPORTED arrangement, not a failure: ghost
    //    text is off, next-edit (Tab) is unaffected. Round-tripping
    //    /v1/completions here would 503 by design, so skip it rather
    //    than manufacture a red rung.
    if (!fimLane) {
      out.appendLine("");
      out.appendLine("SKIP  completion round-trip — this model has no FIM lane, so");
      out.appendLine("      POST /v1/completions returns 503 by design.");
      out.appendLine("");
      out.appendLine("Next-edit suggestions are ready (Tab accepts). Ghost text needs a");
      out.appendLine("coder GGUF (Mellum2, Qwen2.5-Coder) at [models.edit].path.");
      out.show(true);
      return;
    }

    // 4. Round-trip a synthetic completion.
    try {
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), 15_000);
      const r = await completeFim(
        endpoint,
        { prefix: "fn main() {\n    ", suffix: "\n}", path: "probe.rs", language: "rust" },
        ctrl.signal,
      );
      clearTimeout(timer);
      if (r.text.trim().length === 0) {
        out.appendLine("WARN  round-trip succeeded but the completion was empty");
        out.appendLine(`      finish=${r.finishReason} stop_rule=${r.debug?.stop_rule ?? "?"}`);
      } else {
        out.appendLine(
          `PASS  synthetic completion round-tripped (${r.text.length} chars, ` +
            `ttft=${r.debug?.timings_ms?.ttft ?? "?"}ms)`,
        );
      }
    } catch (err) {
      out.appendLine(`FAIL  completion round-trip: ${(err as Error).message}`);
      out.appendLine("");
      out.appendLine("A 503 here after /status reported a FIM lane means the slot lost it");
      out.appendLine("since boot — the daemon log's [edit] lines name the model and the");
      out.appendLine("fix. Next-edit suggestions are served by a different lane and may");
      out.appendLine("still be working.");
      out.show(true);
      return;
    }

    out.appendLine("");
    out.appendLine("All green — ghost text and next-edit should both be working. If they");
    out.appendLine("aren't, check 'sovereign-fim.disabledLanguages' and the Output > svrn");
    out.appendLine("fim log.");
    out.show(true);
  }

  dispose(): void {
    this.channel.dispose();
  }
}
