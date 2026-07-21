// Glassbox: OutputChannel + ring buffer of the last 20 suggestions
// (extension plan §glassbox). Every daemon response carries
// sovereign_debug — what fed the suggestion, what rule stopped it,
// how long it took. "Explain Last Suggestion" renders the most
// recent record; "Diagnose Completion Setup" runs the sequenced
// probes and prints PASS/FAIL plus the copy-pasteable fix for the
// first failure.

import * as vscode from "vscode";
import { completeFim, probeStatus, SovereignDebug } from "./client";

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
        this.channel.appendLine(`fim_style:    ${d.fim_style ?? "?"}`);
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

    // 2. FIM configured?
    if (!s.fim) {
      out.appendLine("FAIL  no FIM model configured (inference.fim is null)");
      out.appendLine("");
      out.appendLine("Fix: add to ~/.svrnmesh/config.toml:");
      out.appendLine("");
      out.appendLine("  [models.fim]");
      out.appendLine('  path = "/path/to/Qwen2.5-Coder-1.5B.gguf"  # or Mellum2 coder GGUF');
      out.appendLine("");
      out.appendLine("then `sovereign daemon restart`. The model must be a base");
      out.appendLine("(non-instruct) coder with FIM markers — the daemon's boot log");
      out.appendLine("says so explicitly if the vocab probe refused your model.");
      out.show(true);
      return;
    }
    out.appendLine(
      `PASS  FIM slot live: ${s.fim.model_id} (${s.fim.fim_style}) on slot '${s.fim.slot}'`,
    );

    // 3. Round-trip a synthetic completion.
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
    } catch (e) {
      out.appendLine(`FAIL  completion round-trip: ${(e as Error).message}`);
      out.appendLine("");
      out.appendLine("If this is a 503: the FIM slot failed its marker probe at boot —");
      out.appendLine("the daemon log's [fim] lines name the model and the fix.");
      out.show(true);
      return;
    }

    out.appendLine("");
    out.appendLine("All green — ghost text should be working. If it isn't, check");
    out.appendLine("'sovereign-fim.disabledLanguages' and the Output > svrn fim log.");
    out.show(true);
  }

  dispose(): void {
    this.channel.dispose();
  }
}
