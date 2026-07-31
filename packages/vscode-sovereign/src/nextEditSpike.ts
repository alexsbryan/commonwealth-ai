// Next-edit AMBIENT SPIKE (NEXT_EDIT.md §5). THROWAWAY.
//
// Spike 1 proved the accept/jump mechanics (decorations + context-key
// Tab/Esc). Spike 2 — this file — explores the TRIGGER: the system
// watches edits continuously, induces a literal rewrite rule from the
// last few coalesced edit units (ruleInduction.ts), and surfaces a
// proposal only when structural confidence crosses the threshold.
//
// Surfacing policy under exploration, the part to feel:
//   - never scroll the viewport uninvited: if the next site is
//     visible, decorate it in place; if off-screen, show a one-line
//     hint at the cursor line's end — the first Tab jumps, later
//     Tabs accept+advance;
//   - Esc suppresses the rule for the session (no re-nagging);
//   - any manual edit clears the proposal; the next settle
//     re-evaluates.
//
// Still zero daemon traffic; induction runs in-extension. The real
// build moves induction behind POST /v1/edit_predictions (§3) —
// what's being validated here is the trigger + surfacing feel, which
// is client-side either way. Delete with the real provider.

import * as vscode from "vscode";
import {
  ClosedUnit,
  RawChange,
  UnitCoalescer,
  unitsFromMultiChange,
} from "./editUnits";
import { chooseScenario, findGuardedSites } from "./nextEditSpikeCore";
import {
  expandRule,
  GuardedRule,
  induce,
  ruleKey,
  shouldFire,
} from "./ruleInduction";

const CONTEXT_KEY = "sovereignFim.nextEditVisible";

interface SpikeSession {
  editor: vscode.TextEditor;
  rule: GuardedRule;
  /** hint = surfaced but not engaged (site may be off-screen). */
  stage: "hint" | "engaged";
  /** The old-text range proposed for replacement (engaged only). */
  site: vscode.Range | null;
  applied: number;
}

interface TrackedDoc {
  uri: string;
  /** Shadow of the document text, needed to recover deleted text —
   *  contentChanges carry the range but not what it used to say. */
  text: string;
}

export class NextEditSpike implements vscode.Disposable {
  private session: SpikeSession | null = null;
  private tracked: TrackedDoc | null = null;
  private readonly coalescer = new UnitCoalescer();
  /** Induced rule per closed unit, oldest first (null = uninducible). */
  private rules: Array<GuardedRule | null> = [];
  private readonly suppressed = new Set<string>();
  private settleTimer: ReturnType<typeof setTimeout> | null = null;
  private applyingEdit = false;
  private readonly oldTextDeco: vscode.TextEditorDecorationType;
  private readonly hintDeco: vscode.TextEditorDecorationType;
  private readonly subs: vscode.Disposable[] = [];

  constructor() {
    this.oldTextDeco = vscode.window.createTextEditorDecorationType({
      backgroundColor: new vscode.ThemeColor("diffEditor.removedTextBackground"),
      textDecoration: "line-through",
    });
    this.hintDeco = vscode.window.createTextEditorDecorationType({});
    this.subs.push(
      vscode.workspace.onDidChangeTextDocument((e) => this.onChange(e)),
      vscode.window.onDidChangeActiveTextEditor((editor) => {
        this.clearSurface();
        this.track(editor?.document);
      }),
    );
    this.track(vscode.window.activeTextEditor?.document);
  }

  // ---- ambient pipeline ------------------------------------------------

  private onChange(e: vscode.TextDocumentChangeEvent): void {
    if (e.contentChanges.length === 0) return;
    if (!this.tracked || e.document.uri.toString() !== this.tracked.uri) {
      // First event on a newly-active doc: adopt it (post-change) and
      // sacrifice this event's units.
      if (e.document === vscode.window.activeTextEditor?.document) {
        this.track(e.document);
      }
      return;
    }

    const pre = this.tracked.text;
    const raws: RawChange[] = e.contentChanges.map((c) => ({
      offset: c.rangeOffset,
      deleted: pre.slice(c.rangeOffset, c.rangeOffset + c.rangeLength),
      inserted: c.text,
    }));
    let post = pre;
    for (const c of [...raws].sort((a, b) => b.offset - a.offset)) {
      post = post.slice(0, c.offset) + c.inserted + post.slice(c.offset + c.deleted.length);
    }
    this.tracked.text = post;

    if (this.applyingEdit) return; // our own accept — snapshot only

    if (raws.length === 1) {
      this.recordUnit(this.coalescer.feed(raws[0], pre), pre);
    } else {
      // Multi-cursor event: close any open burst, then each change is
      // its own unit — N simultaneous identical edits are N supports.
      this.recordUnit(this.coalescer.settle(pre), pre);
      for (const u of unitsFromMultiChange(raws)) this.recordUnit(u, post);
    }

    this.clearSurface(); // stale against the new text; settle re-evaluates
    if (this.settleTimer) clearTimeout(this.settleTimer);
    this.settleTimer = setTimeout(() => this.onSettle(), this.settleMs());
  }

  private onSettle(): void {
    if (!this.tracked) return;
    this.recordUnit(this.coalescer.settle(this.tracked.text), this.tracked.text);
    this.evaluate();
  }

  private recordUnit(unit: ClosedUnit | null, docAtClose: string): void {
    if (!unit) return;
    this.rules.push(expandRule(docAtClose, unit.start, unit.before, unit.after));
    if (this.rules.length > 32) this.rules.splice(0, this.rules.length - 32);
  }

  private evaluate(): void {
    if (!this.ambientEnabled() || !this.tracked) return;
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.toString() !== this.tracked.uri) return;
    const ind = induce(this.rules);
    if (!ind || this.suppressed.has(ruleKey(ind.rule))) return;

    const text = editor.document.getText();
    const from = editor.document.offsetAt(editor.selection.active);
    const sites = findGuardedSites(
      text, ind.rule.find, from, ind.rule.guardLeft, ind.rule.guardRight,
    );
    if (!shouldFire(ind.rule, ind.support, sites.length)) return;

    const site = this.rangeAt(editor, sites[0], ind.rule.find.length);
    const visible = editor.visibleRanges.some((v) => v.intersection(site) !== undefined);
    if (visible) {
      this.engage(editor, ind.rule, site, sites.length, 0, false);
    } else {
      this.session = { editor, rule: ind.rule, stage: "hint", site: null, applied: 0 };
      const cur = editor.document.lineAt(editor.selection.active.line).range.end;
      editor.setDecorations(this.hintDeco, [
        {
          range: new vscode.Range(cur, cur),
          renderOptions: {
            after: {
              contentText: `⇥ ${label(ind.rule)} · ${sites.length} site${sites.length === 1 ? "" : "s"} · next: line ${site.start.line + 1}`,
              color: new vscode.ThemeColor("editorGhostText.foreground"),
              margin: "0 0 0 2ch",
            },
          },
        },
      ]);
      void vscode.commands.executeCommand("setContext", CONTEXT_KEY, true);
    }
  }

  // ---- proposal lifecycle ----------------------------------------------

  private engage(
    editor: vscode.TextEditor,
    rule: GuardedRule,
    site: vscode.Range,
    remaining: number,
    applied: number,
    reveal: boolean,
  ): void {
    this.session = { editor, rule, stage: "engaged", site, applied };
    if (reveal) {
      editor.revealRange(site, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
    }
    editor.setDecorations(this.oldTextDeco, [
      {
        range: site,
        renderOptions: {
          after: {
            contentText: rule.replace,
            color: new vscode.ThemeColor("editorGhostText.foreground"),
            backgroundColor: new vscode.ThemeColor("diffEditor.insertedTextBackground"),
            margin: "0 0 0 0.5ch",
          },
        },
      },
    ]);
    const lineEnd = editor.document.lineAt(site.end.line).range.end;
    editor.setDecorations(this.hintDeco, [
      {
        range: new vscode.Range(lineEnd, lineEnd),
        renderOptions: {
          after: {
            contentText: `⇥ accept · esc dismiss · ${remaining} remaining`,
            color: new vscode.ThemeColor("editorGhostText.foreground"),
            margin: "0 0 0 2ch",
          },
        },
      },
    ]);
    void vscode.commands.executeCommand("setContext", CONTEXT_KEY, true);
  }

  /** Tab keybinding: engage from a hint, or apply + advance. */
  async accept(): Promise<void> {
    const s = this.session;
    if (!s) return;
    const doc = s.editor.document;

    if (s.stage === "hint") {
      const from = doc.offsetAt(s.editor.selection.active);
      const sites = this.sitesFor(s.rule, doc.getText(), from);
      if (sites.length === 0) {
        this.clearSurface();
        return;
      }
      this.engage(s.editor, s.rule, this.rangeAt(s.editor, sites[0], s.rule.find.length), sites.length, s.applied, true);
      return;
    }

    const site = s.site as vscode.Range;
    this.applyingEdit = true;
    let ok = false;
    try {
      ok = await s.editor.edit((b) => b.replace(site, s.rule.replace));
    } finally {
      this.applyingEdit = false;
    }
    if (!ok) {
      this.clearSurface();
      return;
    }
    const applied = s.applied + 1;
    const from = doc.offsetAt(site.start) + s.rule.replace.length;
    const sites = this.sitesFor(s.rule, doc.getText(), from);
    if (sites.length === 0) {
      this.clearSurface();
      vscode.window.setStatusBarMessage(
        `svrn fim spike: done — ${applied} edit${applied === 1 ? "" : "s"} applied`,
        4000,
      );
      return;
    }
    // Mid-chain the user is committed: revealing is expected here,
    // unlike at first surface.
    this.engage(s.editor, s.rule, this.rangeAt(s.editor, sites[0], s.rule.find.length), sites.length, applied, true);
  }

  /** Esc keybinding: dismiss AND suppress the rule for the session. */
  dismiss(): void {
    if (this.session) this.suppressed.add(ruleKey(this.session.rule));
    this.clearSurface();
  }

  /** Manual demo entry (spike 1 flow): hardwired stand-in scenario. */
  start(): void {
    this.clearSurface();
    const editor = vscode.window.activeTextEditor;
    if (!editor) {
      void vscode.window.showInformationMessage("svrn fim spike: open a file first.");
      return;
    }
    const doc = editor.document;
    const wordRange = doc.getWordRangeAtPosition(editor.selection.active);
    const scenario = chooseScenario(doc.getText(), wordRange ? doc.getText(wordRange) : null);
    if (!scenario) {
      void vscode.window.showInformationMessage(
        "svrn fim spike: nothing to demo — add some console.log( lines, or put the cursor on a repeated word.",
      );
      return;
    }
    const rule: GuardedRule = {
      ...scenario.rule,
      guardLeft: scenario.wholeWord,
      guardRight: scenario.wholeWord,
    };
    const sites = this.sitesFor(rule, doc.getText(), doc.offsetAt(editor.selection.active));
    if (sites.length === 0) return;
    this.engage(editor, rule, this.rangeAt(editor, sites[0], rule.find.length), sites.length, 0, true);
  }

  private clearSurface(): void {
    const s = this.session;
    this.session = null;
    if (s) {
      s.editor.setDecorations(this.oldTextDeco, []);
      s.editor.setDecorations(this.hintDeco, []);
    }
    void vscode.commands.executeCommand("setContext", CONTEXT_KEY, false);
  }

  // ---- plumbing --------------------------------------------------------

  private track(doc: vscode.TextDocument | undefined): void {
    const trackable =
      doc && (doc.uri.scheme === "file" || doc.uri.scheme === "untitled");
    this.tracked = trackable ? { uri: doc.uri.toString(), text: doc.getText() } : null;
    this.coalescer.reset();
    this.rules = [];
    if (this.settleTimer) clearTimeout(this.settleTimer);
  }

  private sitesFor(rule: GuardedRule, text: string, from: number): number[] {
    return findGuardedSites(text, rule.find, from, rule.guardLeft, rule.guardRight);
  }

  private rangeAt(editor: vscode.TextEditor, offset: number, len: number): vscode.Range {
    return new vscode.Range(
      editor.document.positionAt(offset),
      editor.document.positionAt(offset + len),
    );
  }

  private ambientEnabled(): boolean {
    return vscode.workspace
      .getConfiguration("sovereign-fim")
      .get<boolean>("nextEditSpike.ambient", true);
  }

  private settleMs(): number {
    return vscode.workspace
      .getConfiguration("sovereign-fim")
      .get<number>("nextEditSpike.settleMs", 600);
  }

  dispose(): void {
    this.clearSurface();
    if (this.settleTimer) clearTimeout(this.settleTimer);
    this.oldTextDeco.dispose();
    this.hintDeco.dispose();
    for (const s of this.subs) s.dispose();
  }
}

function label(rule: GuardedRule): string {
  const t = (s: string) => (s.length > 18 ? `${s.slice(0, 17)}…` : s);
  return `${t(rule.find)} → ${t(rule.replace)}`;
}
