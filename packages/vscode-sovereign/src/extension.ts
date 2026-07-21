// Extension entry (extension plan §extension). Registers the inline
// provider for all file languages, the status bar, and the two
// glassbox commands. All model/slot/context logic is daemon-side;
// this file only wires editor events to the client.

import * as vscode from "vscode";
import { readConfig } from "./config";
import { FimDebugLog } from "./debug";
import { FimProvider } from "./provider";
import { FimStatusBar } from "./statusbar";

export function activate(context: vscode.ExtensionContext): void {
  const log = new FimDebugLog();
  const statusBar = new FimStatusBar(() => readConfig().endpoint);
  const provider = new FimProvider(log, statusBar);

  context.subscriptions.push(
    log,
    statusBar,
    vscode.languages.registerInlineCompletionItemProvider(
      { pattern: "**" },
      provider,
    ),
    vscode.commands.registerCommand("sovereign-fim.explainLastSuggestion", () => {
      log.explainLast();
    }),
    vscode.commands.registerCommand("sovereign-fim.diagnose", () => {
      void log.diagnose(readConfig().endpoint);
    }),
  );

  statusBar.start();
}

export function deactivate(): void {
  // Subscriptions dispose via the context; nothing async to await.
}
