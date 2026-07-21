// Pure prefix/suffix window capture (extension plan §context).
// The prefix is front-truncated at LINE BOUNDARIES: the oldest line
// is always complete, so the token sequence at the tail of the
// prompt stays identical as the user types — that's what makes the
// daemon's prefix cache hit (a mid-line truncation would change the
// first tokens of every request and thrash the cache).

export interface FimContext {
  prefix: string;
  suffix: string;
}

export function captureContext(
  documentText: string,
  offset: number,
  maxPrefixLines: number,
  maxSuffixLines: number,
): FimContext {
  const before = documentText.slice(0, offset);
  const after = documentText.slice(offset);

  const beforeLines = before.split("\n");
  // The last element is the partial line up to the cursor — always
  // included. Truncation drops WHOLE oldest lines only.
  const prefixLines = beforeLines.slice(-maxPrefixLines);
  const prefix = prefixLines.join("\n");

  const afterLines = after.split("\n");
  const suffixLines = afterLines.slice(0, maxSuffixLines);
  const suffix = suffixLines.join("\n");

  return { prefix, suffix };
}
