// Extension entry (extension plan §extension). Registers the inline
// provider for all file languages, the status bar, and the two
// glassbox commands. All model/slot/context logic is daemon-side;
// this file only wires editor events to the client.

import * as vscode from "vscode";
import { readConfig } from "./config";
import { FimDebugLog } from "./debug";
import { NextEditSpike } from "./nextEditSpike";
import { FimProvider } from "./provider";
import { FimStatusBar } from "./statusbar";

export function activate(context: vscode.ExtensionContext): void {
  const log = new FimDebugLog();
  const statusBar = new FimStatusBar(() => readConfig().endpoint);
  const provider = new FimProvider(log, statusBar);
  // Throwaway render spike (NEXT_EDIT.md §5) — inert until its
  // command is run; delete when the real next-edit provider lands.
  const spike = new NextEditSpike();

  context.subscriptions.push(
    log,
    statusBar,
    spike,
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
    vscode.commands.registerCommand("sovereign-fim.spike.nextEdit", () => {
      spike.start();
    }),
    vscode.commands.registerCommand("sovereign-fim.spike.acceptNextEdit", () => {
      void spike.accept();
    }),
    vscode.commands.registerCommand("sovereign-fim.spike.dismissNextEdit", () => {
      spike.dismiss();
    }),
  );

  statusBar.start();
}

export function deactivate(): void {
  // Subscriptions dispose via the context; nothing async to await.
}
