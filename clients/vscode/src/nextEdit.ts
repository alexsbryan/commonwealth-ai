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
import { CallSiteNavigator } from "./callSites";
import {
  DaemonError,
  HistoryUnitWire,
  NextEditOutcome,
  predictEdits,
  reportOutcome,
} from "./client";
import { readConfig } from "./config";
import { buildQueue, QueuedEdit, shiftAfterApply } from "./editQueue";
import { RawChange, UnitCoalescer, unitsFromMultiChange } from "./editUnits";
import {
  MAX_HISTORY,
  MAX_TEXT_BYTES,
  bytes,
  sliceWhole,
  unitFitsWire,
} from "./wireLimits";

const CONTEXT_KEY = "sovereignFim.nextEditVisible";
/** Untouched context captured per side of a unit at close — enough
 *  for the daemon's expansion (its MAX_CTX is 40 chars). */
const UNIT_CTX_CHARS = 48;


interface Session {
  editor: vscode.TextEditor;
  queue: QueuedEdit[];
  ruleKey: string;
  hint: string;
  stage: "hint" | "engaged";
  applied: number;
  /** Document version the queue's offsets are valid against. */
  version: number;
  /** The daemon's id for the prediction behind this session. Empty when
   *  the daemon predates the outcome route. */
  episodeId: string;
  /** At-most-once guard for the outcome report. A queue is one episode
   *  however many Tabs walk it, so the FIRST resolution is the outcome
   *  and later ones are the same story told twice. */
  reported: boolean;
}

/** The workspace folder `doc` belongs to, or `null` when it is outside
 *  every folder (a scratch file). The symbol lane needs it to bridge
 *  the graph's repo-relative paths to disk, and there is nothing sane
 *  to assume when it is absent. */
function workspaceRootFor(doc: vscode.TextDocument): string | null {
  return vscode.workspace.getWorkspaceFolder(doc.uri)?.uri.fsPath ?? null;
}

/** The symbol lane's request fields, or nothing at all.
 *
 *  All three must be present together: the daemon will not guess the
 *  corpus (enumerating installed indexes opens every one on disk) and
 *  cannot bridge an absolute editor path to the graph's repo-relative
 *  keys without the root. Asking for the lane without them would spend
 *  a round trip to be told `graph_unavailable`. */
function symbolLaneRequest(
  doc: vscode.TextDocument,
  hasSurface: boolean,
): { symbol_lane: true; corpus_id: string; workspace_root: string } | Record<string, never> {
  if (!hasSurface) return {};
  const cfg = vscode.workspace.getConfiguration("sovereign-fim");
  if (!cfg.get<boolean>("nextEdit.symbolLane", true)) return {};
  const root = workspaceRootFor(doc);
  if (root === null) return {};
  // Defaults to the workspace folder's own name, which is how
  // `svrn code index` labels a repo corpus.
  const corpusId =
    cfg.get<string>("nextEdit.corpusId", "") || root.split("/").filter(Boolean).pop() || "";
  if (corpusId === "") return {};
  return { symbol_lane: true, corpus_id: corpusId, workspace_root: root };
}

export class NextEditController implements vscode.Disposable {
  /** The symbol lane's surface. Injected rather than constructed here
   *  so `extension.ts` owns its lifetime alongside every other
   *  disposable, and so a test can drive the controller without a
   *  status bar. */
  private navigator: CallSiteNavigator | null = null;
  private session: Session | null = null;
  private tracked: { uri: string; text: string } | null = null;
  private readonly coalescer = new UnitCoalescer();
  private units: HistoryUnitWire[] = [];
  private readonly suppressed = new Set<string>();
  private settleTimer: ReturnType<typeof setTimeout> | null = null;
  private applyingEdit = false;
  private warnedBadRequest = false;
  private inflight: AbortController | null = null;
  private readonly oldTextDeco: vscode.TextEditorDecorationType;
  private readonly hintDeco: vscode.TextEditorDecorationType;
  private readonly subs: vscode.Disposable[] = [];

  /** Attach the call-site surface. Optional: with none attached the
   *  symbol lane is simply not requested. */
  setNavigator(nav: CallSiteNavigator): void {
    this.navigator = nav;
  }

  constructor() {
    this.oldTextDeco = vscode.window.createTextEditorDecorationType({
      backgroundColor: new vscode.ThemeColor("diffEditor.removedTextBackground"),
      textDecoration: "line-through",
    });
    this.hintDeco = vscode.window.createTextEditorDecorationType({});
    this.subs.push(
      vscode.workspace.onDidChangeTextDocument((e) => this.onChange(e)),
      vscode.window.onDidChangeActiveTextEditor((editor) => {
        // The developer went somewhere else. That is not a verdict on
        // the suggestion, so the episode stays UNREPORTED and lands in
        // the daemon's `unknown` bucket.
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

    // Undo/redo is not a pattern the user is establishing — it is one
    // they are retracting. Recording it would feed the induction the
    // mirror image of the edit it just learned from (and undoing an
    // accepted suggestion would teach it to propose the reverse), so
    // the snapshot stays current but history does not grow.
    if (e.reason !== undefined) {
      this.coalescer.reset();
      // Undo/redo moved the document out from under the prediction. The
      // sites it named may not even exist any more — diverged, not
      // dismissed.
      this.clearSurface("diverged");
      return;
    }

    if (raws.length === 1) {
      this.recordUnit(this.coalescer.feed(raws[0], pre), pre);
    } else {
      this.recordUnit(this.coalescer.settle(pre), pre);
      for (const u of unitsFromMultiChange(raws)) this.recordUnit(u, post);
    }

    this.inflight?.abort();
    // The developer kept typing past a live proposal. This is the most
    // common non-accept ending BY FAR, and it is the reason the outcome
    // set is four-way: they may have judged it and moved on, or never
    // looked at it. Calling that a dismissal would inflate the
    // acceptance rate's denominator with episodes nobody judged.
    this.clearSurface("diverged");
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
    const after = unit.start + unit.after.length;
    const left = sliceWhole(docAtClose, Math.max(0, unit.start - UNIT_CTX_CHARS), unit.start);
    const right = sliceWhole(docAtClose, after, after + UNIT_CTX_CHARS);
    // A unit the daemon would refuse is worse than no unit: the 400 is
    // per-REQUEST, so keeping it would kill every later prediction too.
    if (!unitFitsWire(unit.before, unit.after, left, right)) {
      return;
    }
    this.units.push({ before: unit.before, after: unit.after, left, right });
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
    // Honour the same opt-out FIM uses: a language the operator has
    // excluded from ghost text should not get tab-through edits either.
    const off = cfg.get<string[]>("disabledLanguages", []);
    if (off.includes(editor.document.languageId)) return;
    if (this.units.length < 2) return; // support needs two edits; save the roundtrip

    const doc = editor.document;
    const text = doc.getText();
    if (bytes(text) > MAX_TEXT_BYTES) return;

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
          // The symbol lane needs to know WHICH graph and WHERE the
          // source is: the daemon deliberately guesses neither.
          // Without both, or without the surface to show a jump list
          // on, the lane is not requested at all.
          ...symbolLaneRequest(doc, this.navigator !== null),
        },
        ctrl.signal,
      );
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      // An unreachable daemon must never nag on the typing path — the
      // FIM status bar already tells that story. A 4xx is the opposite
      // case: the daemon is up and healthy-looking while it refuses
      // everything we send, which means WE built a bad request. Left
      // silent, the lane dies invisibly behind a green status bar.
      // History is the only state that can carry a request-shaped
      // fault forward, so drop it and say so, once.
      const status = e instanceof DaemonError ? e.status : undefined;
      if (status !== undefined && status >= 400 && status < 500) {
        this.units = [];
        this.coalescer.reset();
        if (!this.warnedBadRequest) {
          this.warnedBadRequest = true;
          console.error("next-edit: daemon refused the request, edit history cleared:", e);
          vscode.window.setStatusBarMessage(
            "svrn fim: next-edit history reset (daemon refused a request)",
            4000,
          );
        }
        return;
      }
      if (!(e instanceof DaemonError)) console.error("next-edit:", e);
      return;
    }
    if (ctrl.signal.aborted || doc.version !== version) return;
    // BEFORE the empty-edits return, deliberately. The rule lane is
    // silent by construction on the shape the symbol lane fires on — a
    // signature fanout's trigger and its consequence are different
    // text, so no rule ever reaches support 2 (NEXT_EDIT_SYMBOL_LANE.md)
    // — which means navigation arrives precisely when `edits` is empty.
    // Updating after the return would surface it never.
    this.navigator?.update(result.navigation, workspaceRootFor(doc) ?? "");
    if (result.edits.length === 0) return;
    // Model proposals have no rule_key; suppress per detected SHAPE
    // (the gate's reason) rather than per needle. The needle is the
    // longest common substring of the last two edits' surroundings, so
    // it is near-unique per proposal — keying on it means Esc suppresses
    // something that will never be asked again, and the user keeps
    // dismissing the same category forever.
    const ruleKey =
      result.debug?.rule_key ??
      (result.engine === "model" ? `model:${result.debug?.model?.reason ?? "pattern"}` : "");
    if (this.suppressed.has(ruleKey)) return;

    const queue = buildQueue(text, result.edits);
    const hint =
      result.engine === "model"
        ? `model · ${result.debug?.model?.reason ?? "pattern"}`
        : `${trunc(result.debug?.rule_find)} → ${trunc(result.debug?.rule_replace)}`;
    const first = this.rangeOf(doc, queue[0]);
    const visible = editor.visibleRanges.some((v) => v.intersection(first) !== undefined);
    // A live proposal is being replaced by this one. Reported as
    // superseded so it is not silently lost to `unknown` — the developer
    // never got to judge it, and that is a fact about our own timing,
    // not about them.
    if (this.session) this.report(this.session, "superseded");
    this.session = {
      editor,
      queue,
      ruleKey,
      hint,
      stage: visible ? "engaged" : "hint",
      applied: 0,
      version,
      episodeId: result.episodeId,
      reported: false,
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
      // The developer WANTED this edit — they pressed Tab — but the
      // document no longer matches what was predicted. Reported as
      // diverged, which is the point of having the bucket: it is
      // neither an acceptance nor a rejection.
      this.clearSurface("diverged");
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
      // A newer prediction arrived while the edit was applying, or the
      // editor refused it. The first is `superseded`; the second we
      // cannot characterize, so we say nothing.
      const replaced = this.session !== s;
      if (replaced) this.report(s, "superseded");
      this.clearSurface();
      return;
    }
    // The edit landed. This is the only accept signal, and it fires on
    // the FIRST applied edit of the queue — walking the rest with Tab is
    // the same acceptance, not four more.
    this.report(s, "accepted");
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
    // The one unambiguous rejection: the developer looked at it and said
    // no. Together with `accepted` this is the whole judged population.
    this.clearSurface("dismissed");
  }

  /** Tear the surface down, and say what became of the episode.
   *
   *  The `outcome` argument is REQUIRED to be considered at every call
   *  site — omitting it means "we genuinely do not know", which the
   *  daemon counts as `unknown` rather than folding into a dismissal.
   *  Passing an outcome we cannot stand behind is the failure this
   *  signature is shaped to prevent: an acceptance rate is only as
   *  honest as its denominator. */
  private clearSurface(outcome?: NextEditOutcome): void {
    const s = this.session;
    this.session = null;
    if (s) {
      if (outcome) this.report(s, outcome);
      s.editor.setDecorations(this.oldTextDeco, []);
      s.editor.setDecorations(this.hintDeco, []);
    }
    void vscode.commands.executeCommand("setContext", CONTEXT_KEY, false);
  }

  /** Report once per episode, never twice, never at all if the daemon
   *  gave us no id. Nothing here can throw or surface anything. */
  private report(s: Session, outcome: NextEditOutcome): void {
    if (s.reported || !s.episodeId) return;
    s.reported = true;
    reportOutcome(readConfig().endpoint, s.episodeId, outcome);
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
    // Window closing. Whatever was on screen never got a verdict, and
    // an in-flight POST would not survive teardown anyway — unreported,
    // hence `unknown`.
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

