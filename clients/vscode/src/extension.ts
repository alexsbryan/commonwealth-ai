// Extension entry (extension plan §extension). Registers the inline
// provider for all file languages, the status bar, and the two
// glassbox commands. All model/slot/context logic is daemon-side;
// this file only wires editor events to the client.

import * as vscode from "vscode";
import { CallSiteNavigator } from "./callSites";
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
  // The symbol lane's surface: a jump list for the call sites of a
  // function whose signature is being edited. Proposes no text — see
  // `callSites.ts` for the measurement that makes navigation the
  // affordance and edits the thing still waiting on a number.
  const callSites = new CallSiteNavigator();
  nextEdit.setNavigator(callSites);

  context.subscriptions.push(
    log,
    statusBar,
    nextEdit,
    callSites,
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
    vscode.commands.registerCommand("sovereign-fim.callSites.show", () => {
      void callSites.show();
    }),
  );

  statusBar.start();
}

export function deactivate(): void {
  // Subscriptions dispose via the context; nothing async to await.
}
