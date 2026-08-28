// The symbol lane's surface: a jump list for the call sites of a
// function whose signature you are editing.
//
// WHY A JUMP LIST AND NOT AN EDIT. Measured on the index-aligned bank
// (`gym/next-edit/aligned/`, M1a, 2026-08-28), on the shape this fires
// on — an existing function whose parameter list changed — the graph's
// site RECALL is 95.8%, cluster-bootstrap 95% CI [87.0, 100.0], clear
// of the pre-registered 80% bar. Site PRECISION is 69.7% with a CI of
// [34.4, 91.5]: the 60% bar sits inside the interval, so precision is a
// could-not-judge rather than a pass. Navigation is the affordance
// whose bar is recall — a wrong entry costs one keystroke — while
// proposing text at each site would be spending a precision number
// nobody has. When that measurement lands, this is where edits attach.
//
// The surface is deliberately QUIET: a status-bar item that appears
// when there are sites and nothing else. Next-edit already owns the
// inline lane; a second thing competing for the same moment would make
// both worse.

import * as vscode from "vscode";
import type { CallSite, Navigation } from "./client";

export class CallSiteNavigator implements vscode.Disposable {
  private readonly item: vscode.StatusBarItem;
  private sites: CallSite[] = [];
  private symbol = "";
  private root = "";
  private truncated = false;
  /** Why the daemon said nothing, when it said nothing. Kept so the
   *  empty case can tell the truth: "edit a parameter list" is wrong
   *  advice if the real reason is that no index exists. */
  private declined: string | null = null;

  constructor() {
    // Priority below the FIM status item so the two never reorder as
    // this one appears and disappears.
    this.item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 99);
    this.item.command = "sovereign-fim.callSites.show";
    this.item.tooltip = "Call sites of the function whose signature you are editing";
  }

  /** Take the daemon's answer. `null` (older daemon, lane off, or a
   *  decline) clears the surface — there is no stale-list state. */
  update(nav: Navigation | null, workspaceRoot: string): void {
    this.root = workspaceRoot;
    const sites = nav?.sites ?? [];
    this.sites = sites;
    this.symbol = nav?.symbol ?? "";
    this.truncated = nav?.truncated ?? false;
    this.declined = nav?.declined ?? null;
    if (sites.length === 0) {
      this.item.hide();
      return;
    }
    const n = sites.length;
    this.item.text = `$(references) ${n} call site${n === 1 ? "" : "s"}`;
    this.item.show();
  }

  /** Open the list. Selecting an entry moves the cursor; NOTHING is
   *  written — see the header. */
  async show(): Promise<void> {
    if (this.sites.length === 0) {
      // `graph_unavailable` is the one a developer can act on, and it is
      // the one that looks identical to "the feature does not exist":
      // no error, no failed request, just an item that never appears.
      // The daemon warns once in its own log; this is the same fact
      // where the developer actually is.
      if (this.declined === "graph_unavailable") {
        const run = "Show me how";
        const pick = await vscode.window.showWarningMessage(
          "svrn: call-site navigation is off — this workspace has no code index. " +
            "Every other next-edit lane is unaffected.",
          run,
        );
        if (pick === run) {
          const term = vscode.window.createTerminal("svrn index");
          term.show();
          // Sent, NOT run: indexing takes minutes and starting it
          // behind someone's back is not a favour. They press Enter.
          term.sendText("svrn init", false);
          void vscode.window.showInformationMessage(
            "Press Enter to index. Afterwards: `svrn doctor` — read the `scip_indexed` line.",
          );
        }
        return;
      }
      void vscode.window.showInformationMessage(
        this.declined === "unsupported_language"
          ? "svrn: call-site navigation covers Rust today — it needs an indexed symbol graph."
          : "svrn: no call sites — edit a function's parameter list to see them.",
      );
      return;
    }
    const items = this.sites.map((s) => ({
      label: s.preview,
      // 0-based on the wire (the graph's own convention), 1-based here
      // because that is what an editor shows in its gutter.
      description: `${s.path}:${s.line + 1}`,
      site: s,
    }));
    const picked = await vscode.window.showQuickPick(items, {
      title: this.truncated
        ? `Call sites of ${this.symbol} (first ${items.length}, more not shown)`
        : `Call sites of ${this.symbol}`,
      placeHolder: "Jump to a call site — nothing is edited",
      matchOnDescription: true,
    });
    if (!picked) return;
    await this.jump(picked.site);
  }

  private async jump(site: CallSite): Promise<void> {
    const uri = vscode.Uri.file(
      site.path.startsWith("/") ? site.path : `${this.root}/${site.path}`,
    );
    let doc: vscode.TextDocument;
    try {
      doc = await vscode.workspace.openTextDocument(uri);
    } catch {
      // The index describes the last save; a file deleted since is not
      // the developer's problem to debug mid-keystroke.
      void vscode.window.setStatusBarMessage(`svrn: ${site.path} is no longer there`, 3000);
      return;
    }
    const editor = await vscode.window.showTextDocument(doc, { preview: true });
    // Clamp: the graph's line is from the last save and the buffer may
    // be dirty. Landing on the last line beats throwing.
    const line = Math.min(Math.max(site.line, 0), doc.lineCount - 1);
    const col = Math.min(Math.max(site.col, 0), doc.lineAt(line).text.length);
    const pos = new vscode.Position(line, col);
    editor.selection = new vscode.Selection(pos, pos);
    editor.revealRange(new vscode.Range(pos, pos), vscode.TextEditorRevealType.InCenterIfOutsideViewport);
  }

  dispose(): void {
    this.item.dispose();
  }
}
