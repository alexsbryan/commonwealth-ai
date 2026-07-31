// Pure logic for the next-edit render spike (NEXT_EDIT.md §5).
// THROWAWAY: a deterministic stand-in for the predictor, kept pure so
// the site-scan behavior is unit-testable without the vscode API.
// Delete alongside nextEditSpike.ts when the real provider lands.

export interface SpikeRule {
  find: string;
  replace: string;
}

export interface SpikeScenario {
  rule: SpikeRule;
  wholeWord: boolean;
}

/// Occurrence offsets of `find` in `text`, ordered as a tab-through
/// queue: document order starting at `fromOffset`, wrapping to the
/// occurrences before it. Matches never overlap (scan resumes past
/// each match) so applying them in order is always safe.
export function findSites(
  text: string,
  find: string,
  fromOffset: number,
): number[] {
  if (find.length === 0) return [];
  const all: number[] = [];
  let idx = text.indexOf(find);
  while (idx !== -1) {
    all.push(idx);
    idx = text.indexOf(find, idx + find.length);
  }
  const after = all.filter((o) => o >= fromOffset);
  const before = all.filter((o) => o < fromOffset);
  return [...after, ...before];
}

const isWordChar = (ch: string | undefined): boolean =>
  ch !== undefined && /[A-Za-z0-9_]/.test(ch);

/// findSites with identifier-boundary guards, applied per end. A
/// guard on an end that is a word character keeps the match from
/// landing inside a longer identifier — and keeps a rule like
/// `word` → `wordNext` from re-matching its own output.
export function findGuardedSites(
  text: string,
  find: string,
  fromOffset: number,
  guardLeft: boolean,
  guardRight: boolean,
): number[] {
  return findSites(text, find, fromOffset).filter(
    (o) =>
      (!guardLeft || !isWordChar(text[o - 1])) &&
      (!guardRight || !isWordChar(text[o + find.length])),
  );
}

/// Both-ends-guarded variant for the word-rename scenario.
export function findWordSites(
  text: string,
  word: string,
  fromOffset: number,
): number[] {
  return findGuardedSites(text, word, fromOffset, true, true);
}

/// The spike's stand-in "predictor": prefer the canonical
/// console.log → console.debug scenario; otherwise rename the word
/// under the cursor when it repeats. Null means nothing to demo.
export function chooseScenario(
  text: string,
  wordAtCursor: string | null,
): SpikeScenario | null {
  if (text.includes("console.log(")) {
    return {
      rule: { find: "console.log(", replace: "console.debug(" },
      wholeWord: false,
    };
  }
  if (wordAtCursor && wordAtCursor.length > 0) {
    const sites = findWordSites(text, wordAtCursor, 0);
    if (sites.length >= 2) {
      return {
        rule: { find: wordAtCursor, replace: `${wordAtCursor}Next` },
        wholeWord: true,
      };
    }
  }
  return null;
}
