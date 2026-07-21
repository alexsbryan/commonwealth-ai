// Status bar (extension plan §statusbar). Three states, driven by
// GET /status's inference.fim field (works in both alias and
// dedicated modes — the resident-role check alone would miss alias):
//
//   ok       daemon up, FIM slot live   → $(zap) model id
//   noFim    daemon up, no [models.fim] → warning + the exact fix
//   offline  daemon unreachable         → muted, tooltip says so
//
// Probed on activation, every 60s, and after request failures.

import * as vscode from "vscode";
import { probeStatus, StatusProbe } from "./client";

export class FimStatusBar implements vscode.Disposable {
  private item: vscode.StatusBarItem;
  private timer: ReturnType<typeof setInterval> | undefined;
  private probing = false;

  constructor(private readonly getEndpoint: () => string) {
    this.item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    this.item.command = "sovereign-fim.diagnose";
    this.item.show();
  }

  start(): void {
    void this.probe();
    this.timer = setInterval(() => void this.probe(), 60_000);
  }

  /** Re-probe outside the timer (after a request failure). */
  async probe(): Promise<StatusProbe> {
    if (this.probing) return { daemonUp: false, fim: null };
    this.probing = true;
    try {
      const s = await probeStatus(this.getEndpoint());
      this.render(s);
      return s;
    } finally {
      this.probing = false;
    }
  }

  private render(s: StatusProbe): void {
    if (!s.daemonUp) {
      this.item.text = "$(circle-slash) svrn fim";
      this.item.tooltip =
        "svrn daemon unreachable — completions offline.\n" +
        "Start it with `svrn daemon run`, then click to re-diagnose.";
      this.item.backgroundColor = undefined;
      return;
    }
    if (!s.fim) {
      this.item.text = "$(warning) svrn fim";
      this.item.tooltip = new vscode.MarkdownString(
        "Daemon is up but no FIM model is configured.\n\n" +
          "Add to `~/.svrnmesh/config.toml`:\n\n" +
          "```toml\n[models.fim]\npath = \"/path/to/coder-model.gguf\"\n```\n\n" +
          "then `sovereign daemon restart`. Click for the full diagnostic.",
      );
      this.item.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.warningBackground",
      );
      return;
    }
    this.item.text = `$(zap) ${s.fim.model_id}`;
    this.item.tooltip =
      `svrn fim: ${s.fim.model_id} (${s.fim.fim_style})\n` +
      `slot: ${s.fim.slot}${s.fim.aliased_to_fast ? " (shared fast slot — lean mode)" : ""}\n` +
      "Click for details.";
    this.item.backgroundColor = undefined;
  }

  dispose(): void {
    if (this.timer) clearInterval(this.timer);
    this.item.dispose();
  }
}
