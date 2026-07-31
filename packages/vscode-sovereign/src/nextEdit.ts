// Next-edit provider (NEXT_EDIT.md §3-§5) — the production
// descendant of the two render/trigger spikes. The extension is a
// thin capture-and-render shell: it coalesces keystrokes into edit
// units (editUnits.ts), ships them with the document to the daemon
// on edit-settle, and renders whatever queue comes back. ALL policy
// — context expansion, induction, the firing threshold — lives
// daemon-side in /v1/edit_predictions, so other IDE clients inherit
// the behavior for free.
//
// Surfacing contract (operator-validated in the spikes): never
// scroll uninvited — a visible next site decorates in place, an
// off-screen one gets only an end-of-line hint; the first Tab jumps,
// later Tabs accept+advance; Esc suppresses the rule for the
// session; any manual edit clears the proposal and the next settle
// re-evaluates.

import * as vscode from "vscode";
import {
  DaemonError,
  HistoryUnitWire,
  predictEdits,
} from "./client";
import { readConfig } from "./config";
import { buildQueue, QueuedEdit, shiftAfterApply } from "./editQueue";
import { RawChange, UnitCoalescer, unitsFromMultiChange } from "./editUnits";

const CONTEXT_KEY = "sovereignFim.nextEditVisible";
/** Untouched context captured per side of a unit at close — enough
 *  for the daemon's expansion (its MAX_CTX is 40 chars). */
const UNIT_CTX_CHARS = 48;
/** Mirror the daemon's request caps; an over-cap request would 400. */
const MAX_TEXT_BYTES = 512 * 1024;
const MAX_HISTORY = 32;
const MAX_UNIT_CHARS = 2048;

interface Session {
  editor: vscode.TextEditor;
  queue: QueuedEdit[];
  ruleKey: string;
  hint: string;
  stage: "hint" | "engaged";
  applied: number;
  /** Document version the queue's offsets are valid against. */
  version: number;
}

export class NextEditController implements vscode.Disposable {
  private session: Session | null = null;
  private tracked: { uri: string; text: string } | null = null;
  private readonly coalescer = new UnitCoalescer();
  private units: HistoryUnitWire[] = [];
  private readonly suppressed = new Set<string>();
  private settleTimer: ReturnType<typeof setTimeout> | null = null;
  private applyingEdit = false;
  private inflight: AbortController | null = null;
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

  // ---- capture ---------------------------------------------------------

  private onChange(e: vscode.TextDocumentChangeEvent): void {
    if (e.contentChanges.length === 0) return;
    if (!this.tracked || e.document.uri.toString() !== this.tracked.uri) {
      if (e.document === vscode.window.activeTextEditor?.document) {
        this.track(e.document); // adopt post-change; sacrifice this event
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
      this.recordUnit(this.coalescer.settle(pre), pre);
      for (const u of unitsFromMultiChange(raws)) this.recordUnit(u, post);
    }

    this.inflight?.abort();
    this.clearSurface();
    if (this.settleTimer) clearTimeout(this.settleTimer);
    const cfg = vscode.workspace.getConfiguration("sovereign-fim");
    this.settleTimer = setTimeout(
      () => void this.onSettle(),
      cfg.get<number>("nextEdit.settleMs", 600),
    );
  }

  private recordUnit(
    unit: { start: number; before: string; after: string } | null,
    docAtClose: string,
  ): void {
    if (!unit) return;
    if (unit.before.length > MAX_UNIT_CHARS || unit.after.length > MAX_UNIT_CHARS) {
      return; // a paste, not an edit unit — the daemon would refuse it
    }
    this.units.push({
      before: unit.before,
      after: unit.after,
      left: docAtClose.slice(Math.max(0, unit.start - UNIT_CTX_CHARS), unit.start),
      right: docAtClose.slice(
        unit.start + unit.after.length,
        unit.start + unit.after.length + UNIT_CTX_CHARS,
      ),
    });
    if (this.units.length > MAX_HISTORY) {
      this.units.splice(0, this.units.length - MAX_HISTORY);
    }
  }

  // ---- predict + surface ----------------------------------------------

  private async onSettle(): Promise<void> {
    if (!this.tracked) return;
    this.recordUnit(this.coalescer.settle(this.tracked.text), this.tracked.text);

    const cfg = vscode.workspace.getConfiguration("sovereign-fim");
    if (!cfg.get<boolean>("nextEdit.enable", true)) return;
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.toString() !== this.tracked.uri) return;
    if (this.units.length < 2) return; // support needs two edits; save the roundtrip

    const doc = editor.document;
    const text = doc.getText();
    if (Buffer.byteLength(text, "utf8") > MAX_TEXT_BYTES) return;

    this.inflight?.abort();
    const ctrl = new AbortController();
    this.inflight = ctrl;
    const version = doc.version;

    let result;
    try {
      result = await predictEdits(
        readConfig().endpoint,
        {
          history: this.units,
          text,
          cursor: doc.offsetAt(editor.selection.active),
          path: doc.fileName,
          language: doc.languageId,
          model_lane: cfg.get<boolean>("nextEdit.modelLane", true),
        },
        ctrl.signal,
      );
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      // Unreachable daemon must never nag on the typing path; the FIM
      // status bar already tells the story.
      if (!(e instanceof DaemonError)) console.error("next-edit:", e);
      return;
    }
    if (ctrl.signal.aborted || doc.version !== version) return;
    if (result.edits.length === 0) return;
    // Model proposals have no rule_key; suppress per detected pattern
    // (needle) so Esc quiets that pattern, not the whole lane.
    const ruleKey =
      result.debug?.rule_key ??
      (result.engine === "model"
        ? `model:${result.debug?.model?.needle ?? result.debug?.model?.reason ?? ""}`
        : "");
    if (this.suppressed.has(ruleKey)) return;

    const queue = buildQueue(text, result.edits);
    const hint =
      result.engine === "model"
        ? `model · ${result.debug?.model?.reason ?? "pattern"}`
        : `${trunc(result.debug?.rule_find)} → ${trunc(result.debug?.rule_replace)}`;
    const first = this.rangeOf(doc, queue[0]);
    const visible = editor.visibleRanges.some((v) => v.intersection(first) !== undefined);
    this.session = {
      editor,
      queue,
      ruleKey,
      hint,
      stage: visible ? "engaged" : "hint",
      applied: 0,
      version,
    };
    if (visible) {
      this.renderEngaged(false);
    } else {
      const cur = editor.document.lineAt(editor.selection.active.line).range.end;
      editor.setDecorations(this.hintDeco, [
        {
          range: new vscode.Range(cur, cur),
          renderOptions: {
            after: {
              contentText: `⇥ ${hint} · ${queue.length} site${queue.length === 1 ? "" : "s"} · next: line ${first.start.line + 1}`,
              color: new vscode.ThemeColor("editorGhostText.foreground"),
              margin: "0 0 0 2ch",
            },
          },
        },
      ]);
      void vscode.commands.executeCommand("setContext", CONTEXT_KEY, true);
    }
  }

  private renderEngaged(reveal: boolean): void {
    const s = this.session;
    if (!s || s.queue.length === 0) return;
    const site = this.rangeOf(s.editor.document, s.queue[0]);
    if (reveal) {
      s.editor.revealRange(site, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
    }
    s.editor.setDecorations(this.oldTextDeco, [
      {
        range: site,
        renderOptions: {
          after: {
            contentText: s.queue[0].newText,
            color: new vscode.ThemeColor("editorGhostText.foreground"),
            backgroundColor: new vscode.ThemeColor("diffEditor.insertedTextBackground"),
            margin: "0 0 0 0.5ch",
          },
        },
      },
    ]);
    const lineEnd = s.editor.document.lineAt(site.end.line).range.end;
    s.editor.setDecorations(this.hintDeco, [
      {
        range: new vscode.Range(lineEnd, lineEnd),
        renderOptions: {
          after: {
            contentText: `⇥ accept · esc dismiss · ${s.queue.length} remaining`,
            color: new vscode.ThemeColor("editorGhostText.foreground"),
            margin: "0 0 0 2ch",
          },
        },
      },
    ]);
    void vscode.commands.executeCommand("setContext", CONTEXT_KEY, true);
  }

  // ---- accept / dismiss ------------------------------------------------

  /** Tab keybinding: engage from a hint, or apply + advance. */
  async accept(): Promise<void> {
    const s = this.session;
    if (!s) return;

    if (s.stage === "hint") {
      s.stage = "engaged";
      this.renderEngaged(true);
      return;
    }

    const edit = s.queue[0];
    const doc = s.editor.document;
    const site = this.rangeOf(doc, edit);
    if (doc.getText(site) !== edit.oldText) {
      this.clearSurface(); // document diverged from the prediction
      return;
    }
    this.applyingEdit = true;
    let ok = false;
    try {
      ok = await s.editor.edit((b) => b.replace(site, edit.newText));
    } finally {
      this.applyingEdit = false;
    }
    if (!ok || this.session !== s) {
      this.clearSurface();
      return;
    }
    s.applied += 1;
    s.queue = shiftAfterApply(s.queue.slice(1), edit);
    if (s.queue.length === 0) {
      const n = s.applied;
      this.clearSurface();
      vscode.window.setStatusBarMessage(
        `svrn fim: next edit done — ${n} edit${n === 1 ? "" : "s"} applied`,
        4000,
      );
      return;
    }
    this.renderEngaged(true); // mid-chain the user is committed
  }

  /** Esc keybinding: dismiss AND suppress the rule for the session. */
  dismiss(): void {
    if (this.session) this.suppressed.add(this.session.ruleKey);
    this.clearSurface();
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
    const trackable = doc && (doc.uri.scheme === "file" || doc.uri.scheme === "untitled");
    this.tracked = trackable ? { uri: doc.uri.toString(), text: doc.getText() } : null;
    this.coalescer.reset();
    this.units = [];
    this.inflight?.abort();
    if (this.settleTimer) clearTimeout(this.settleTimer);
  }

  private rangeOf(doc: vscode.TextDocument, e: QueuedEdit): vscode.Range {
    return new vscode.Range(doc.positionAt(e.start), doc.positionAt(e.end));
  }

  dispose(): void {
    this.clearSurface();
    this.inflight?.abort();
    if (this.settleTimer) clearTimeout(this.settleTimer);
    this.oldTextDeco.dispose();
    this.hintDeco.dispose();
    for (const s of this.subs) s.dispose();
  }
}

function trunc(s: string | null | undefined): string {
  const v = s ?? "?";
  return v.length > 18 ? `${v.slice(0, 17)}…` : v;
}
