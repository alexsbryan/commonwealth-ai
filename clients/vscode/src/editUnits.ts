// Edit-unit coalescing for next-edit prediction (NEXT_EDIT.md §3):
// keystroke-level document changes are coalesced into semantic
// "edit units" — one contiguous burst of typing/deleting becomes one
// {before, after} replacement, the capture half of the history the
// daemon's rule induction consumes.
//
// Pure: the controller owns the vscode listeners and the document
// snapshot; this module only does the coordinate arithmetic. The
// invariant that makes it simple: every change in a unit happens at
// or after the unit's span start, so the span start is a valid
// offset in BOTH the unit-open snapshot and the current text.

export interface RawChange {
  /** Offset in the document BEFORE this change was applied. */
  offset: number;
  /** Exact text removed (empty for pure insertion). */
  deleted: string;
  /** Exact text inserted (empty for pure deletion). */
  inserted: string;
}

export interface ClosedUnit {
  /** Span start — valid in the document text the unit closed against. */
  start: number;
  before: string;
  after: string;
}

export class UnitCoalescer {
  /** Full document text when the current unit opened; null = no open unit. */
  private base: string | null = null;
  private s = 0;
  private e = 0;
  private delta = 0;

  /**
   * Feed one change. `docBeforeChange` is the full document text the
   * change's offsets refer to. Returns a unit if this change was too
   * far away to merge and therefore closed the previous one.
   */
  feed(c: RawChange, docBeforeChange: string): ClosedUnit | null {
    let closed: ClosedUnit | null = null;
    if (this.base !== null && !this.merges(c)) {
      closed = this.close(docBeforeChange);
    }
    if (this.base === null) {
      this.base = docBeforeChange;
      this.s = c.offset;
      this.e = c.offset + c.inserted.length;
      this.delta = c.inserted.length - c.deleted.length;
    } else {
      const delEnd = c.offset + c.deleted.length;
      const d = c.inserted.length - c.deleted.length;
      this.s = Math.min(this.s, c.offset);
      this.e =
        delEnd <= this.e
          ? Math.max(this.e + d, c.offset + c.inserted.length)
          : c.offset + c.inserted.length;
      this.delta += d;
    }
    return closed;
  }

  /** Close the open unit against the current document text, if any. */
  settle(docNow: string): ClosedUnit | null {
    return this.base === null ? null : this.close(docNow);
  }

  reset(): void {
    this.base = null;
  }

  private merges(c: RawChange): boolean {
    return c.offset <= this.e + 1 && c.offset + c.deleted.length >= this.s - 1;
  }

  private close(docNow: string): ClosedUnit | null {
    const base = this.base as string;
    const unit: ClosedUnit = {
      start: this.s,
      before: base.slice(this.s, this.e - this.delta),
      after: docNow.slice(this.s, this.e),
    };
    this.base = null;
    // A burst that ended where it started (type + undo-by-backspace)
    // is not an edit.
    return unit.before === unit.after ? null : unit;
  }
}

/**
 * A single event carrying multiple changes is a multi-cursor edit:
 * each change is its own already-closed unit (the sites are disjoint
 * by construction). Offsets are remapped to the post-event document.
 */
export function unitsFromMultiChange(changes: RawChange[]): ClosedUnit[] {
  const asc = [...changes].sort((a, b) => a.offset - b.offset);
  let delta = 0;
  const out: ClosedUnit[] = [];
  for (const c of asc) {
    if (c.deleted !== c.inserted) {
      out.push({ start: c.offset + delta, before: c.deleted, after: c.inserted });
    }
    delta += c.inserted.length - c.deleted.length;
  }
  return out;
}
