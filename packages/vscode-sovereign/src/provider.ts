// InlineCompletionItemProvider (extension plan §provider).
//
// Pipeline per keystroke: bail-fast gates (disabled language,
// feature off) → debounce (120ms default — earlier keystrokes win)
// → abort-previous (single-flight: at most one request in the air;
// the daemon runs a single inflight permit per FIM slot, so the
// client contract is cancel-before-reissue) → capture window →
// completeFim → one InlineCompletionItem.
//
// Cancellation is load-bearing, not decorative: VSCode's
// CancellationToken maps onto our AbortController, whose signal
// closes the SSE socket; the daemon sees receiver-drop and cancels
// the decode mid-token. That's what keeps a superseded keystroke
// from burning GPU on a completion nobody will see.

import * as vscode from "vscode";
import { completeFim, DaemonError } from "./client";
import { readConfig } from "./config";
import { captureContext } from "./context";
import { FimDebugLog } from "./debug";
import { FimStatusBar } from "./statusbar";

export class FimProvider implements vscode.InlineCompletionItemProvider {
  private abort: AbortController | null = null;

  constructor(
    private readonly log: FimDebugLog,
    private readonly statusBar: FimStatusBar,
  ) {}

  async provideInlineCompletionItems(
    document: vscode.TextDocument,
    position: vscode.Position,
    _context: vscode.InlineCompletionContext,
    token: vscode.CancellationToken,
  ): Promise<vscode.InlineCompletionItem[] | undefined> {
    const cfg = readConfig();
    if (!cfg.enable) return undefined;
    if (cfg.disabledLanguages.includes(document.languageId)) return undefined;
    if (token.isCancellationRequested) return undefined;

    // Debounce: give the typist `debounceMs` to keep going; a newer
    // keystroke retriggers provideInlineCompletionItems and this
    // sleep is superseded by the cancellation token.
    await new Promise<void>((resolve) => {
      const t = setTimeout(resolve, cfg.debounceMs);
      token.onCancellationRequested(() => {
        clearTimeout(t);
        resolve();
      });
    });
    if (token.isCancellationRequested) return undefined;

    // Single-flight: abort whatever we had in the air.
    this.abort?.abort();
    const ctrl = new AbortController();
    this.abort = ctrl;
    token.onCancellationRequested(() => ctrl.abort());

    const cfgNow = readConfig();
    const { prefix, suffix } = captureContext(
      document.getText(),
      document.offsetAt(position),
      cfgNow.maxPrefixLines,
      cfgNow.maxSuffixLines,
    );
    // An empty document has nothing to complete from — the model
    // would just hallucinate a file.
    if (prefix.trim().length === 0) return undefined;

    try {
      const r = await completeFim(
        cfgNow.endpoint,
        {
          prefix,
          suffix,
          path: document.fileName,
          language: document.languageId,
        },
        ctrl.signal,
      );
      this.log.record({
        when: new Date(),
        path: document.fileName,
        language: document.languageId,
        text: r.text,
        finishReason: r.finishReason,
        wallMs: r.wallMs,
        debug: r.debug,
      });
      if (token.isCancellationRequested) return undefined;
      if (r.text.trim().length === 0) return undefined;
      return [
        new vscode.InlineCompletionItem(
          r.text,
          new vscode.Range(position, position),
        ),
      ];
    } catch (e) {
      if ((e as Error).name === "AbortError") return undefined;
      const msg = e instanceof DaemonError ? e.message : String(e);
      this.log.noteError(msg);
      // A failed request often means the daemon went away or the FIM
      // slot was never configured — refresh the status bar so the
      // user sees it without waiting for the 60s tick.
      void this.statusBar.probe();
      return undefined;
    }
  }
}
