// Status bar (extension plan §statusbar). Driven by GET /status's
// inference.edit field (works in both alias and dedicated modes — the
// resident-role check alone would miss alias).
//
// The editing slot has two independent lanes, so "is it working?" has
// five answers, not three:
//
//   offline     daemon unreachable          → $(circle-slash), muted
//   noEdit      up, no editing model at all → $(warning) + the exact fix
//   degraded    next-edit off the resident
//               chat model, nobody picked
//               it                          → $(info) model id + the nudge
//   nextEdit    next-edit only, no FIM lane → $(lightbulb) model id
//   full        both lanes served           → $(zap) model id
//
// Only the first two are faults. A model that serves next-edit but not
// FIM is a SUPPORTED arrangement (FIM needs marker tokens only coder
// models carry), so it gets no warning background — a status bar that
// is permanently orange stops being read.
//
// Probed on activation, every 60s, and after request failures.

import * as vscode from "vscode";
import { probeStatus, servesFim, servesNextEdit, StatusProbe } from "./client";

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
    if (this.probing) return { daemonUp: false, edit: null };
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
    const e = s.edit;
    if (!e) {
      this.item.text = "$(warning) svrn fim";
      this.item.tooltip = new vscode.MarkdownString(
        "Daemon is up but no editing model is available.\n\n" +
          "Add to `~/.svrnmesh/config.toml`:\n\n" +
          "```toml\n[models.edit]\npath = \"/path/to/model.gguf\"\n```\n\n" +
          "then `sovereign daemon restart`. Click for the full diagnostic.",
      );
      this.item.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.warningBackground",
      );
      return;
    }

    const fim = servesFim(e);
    const nextEdit = servesNextEdit(e);
    const detail =
      `**${e.model_id}**\n\n` +
      `- next edit: ${nextEdit ? `\`${e.next_edit_format}\`` : "unavailable"}\n` +
      `- inline completion (FIM): ${fim ? `\`${e.fim_style}\`` : "unavailable"}\n` +
      `- slot: \`${e.slot}\`${e.aliased_to_fast ? " (shared fast slot — lean mode)" : ""}\n\n` +
      // The daemon composes the next step in exactly one place. Showing
      // it verbatim is what stops this extension becoming a fourth voice
      // with a fourth answer to "what should I do about this".
      (e.advice ? `${e.advice}\n\n` : "") +
      "Click for details.";

    if (!nextEdit && !fim) {
      // Neither lane: nothing an editing model exists for actually
      // works. Rare and transitional, but silence here would look like
      // a healthy slot.
      this.item.text = "$(warning) svrn fim";
      this.item.tooltip = new vscode.MarkdownString(
        "Editing model is resident but serves neither lane.\n\n" + detail,
      );
      this.item.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.warningBackground",
      );
      return;
    }

    // Working arrangements — the glyph names which one, because the
    // difference is something the user feels while typing.
    if (e.degraded) {
      // Suggestions work off borrowed chat weights; the trade is
      // latency, and the daemon's `advice` names it.
      this.item.text = `$(info) ${e.model_id}`;
    } else if (!fim) {
      // Next edit only — deliberate, and there will be no ghost text.
      this.item.text = `$(lightbulb) ${e.model_id}`;
    } else {
      this.item.text = `$(zap) ${e.model_id}`;
    }
    this.item.tooltip = new vscode.MarkdownString(detail);
    this.item.backgroundColor = undefined;
  }

  dispose(): void {
    if (this.timer) clearInterval(this.timer);
    this.item.dispose();
  }
}
