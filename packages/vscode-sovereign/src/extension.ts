// Extension entry (extension plan §extension). Registers the inline
// provider for all file languages, the status bar, and the two
// glassbox commands. All model/slot/context logic is daemon-side;
// this file only wires editor events to the client.

import * as vscode from "vscode";
import { readConfig } from "./config";
import { FimDebugLog } from "./debug";
import { NextEditController } from "./nextEdit";
import { FimProvider } from "./provider";
import { FimStatusBar } from "./statusbar";

export function activate(context: vscode.ExtensionContext): void {
  const log = new FimDebugLog();
  const statusBar = new FimStatusBar(() => readConfig().endpoint);
  const provider = new FimProvider(log, statusBar);
  // Next-edit rule lane (NEXT_EDIT.md): ambient watcher, daemon-side
  // policy via POST /v1/edit_predictions.
  const nextEdit = new NextEditController();

  context.subscriptions.push(
    log,
    statusBar,
    nextEdit,
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
    vscode.commands.registerCommand("sovereign-fim.nextEdit.accept", () => {
      void nextEdit.accept();
    }),
    vscode.commands.registerCommand("sovereign-fim.nextEdit.dismiss", () => {
      nextEdit.dismiss();
    }),
  );

  statusBar.start();
}

export function deactivate(): void {
  // Subscriptions dispose via the context; nothing async to await.
}
