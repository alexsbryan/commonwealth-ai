// Configuration access (extension plan §settings). Six settings,
// read fresh on each access so "Restart to apply" is never needed.

import * as vscode from "vscode";

export interface FimConfig {
  enable: boolean;
  endpoint: string;
  debounceMs: number;
  maxPrefixLines: number;
  maxSuffixLines: number;
  disabledLanguages: string[];
}

export function readConfig(): FimConfig {
  const c = vscode.workspace.getConfiguration("sovereign-fim");
  return {
    enable: c.get<boolean>("enable", true),
    endpoint: c.get<string>("endpoint", "http://127.0.0.1:9741").replace(/\/+$/, ""),
    debounceMs: c.get<number>("debounceMs", 120),
    maxPrefixLines: c.get<number>("maxPrefixLines", 60),
    maxSuffixLines: c.get<number>("maxSuffixLines", 20),
    disabledLanguages: c.get<string[]>("disabledLanguages", ["markdown", "plaintext"]),
  };
}
