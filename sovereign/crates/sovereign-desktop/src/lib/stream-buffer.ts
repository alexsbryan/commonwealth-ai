/**
 * Buffers streaming tokens to word boundaries before flushing.
 * Prevents mid-word rendering in the UI.
 */
export class WordBufferedStream {
  private buffer = "";

  /**
   * Push a token into the buffer.
   * Returns flushed text (up to the last word boundary) or null if
   * no word boundary has been reached yet.
   */
  push(token: string): string | null {
    this.buffer += token;
    // Emit complete words (up to the last space or newline).
    const lastSpace = Math.max(
      this.buffer.lastIndexOf(" "),
      this.buffer.lastIndexOf("\n"),
    );
    if (lastSpace > 0) {
      const words = this.buffer.slice(0, lastSpace + 1);
      this.buffer = this.buffer.slice(lastSpace + 1);
      return words;
    }
    return null;
  }

  /** Flush any remaining buffered text. Call on stream completion. */
  flush(): string {
    const remaining = this.buffer;
    this.buffer = "";
    return remaining;
  }

  /** Reset the buffer for a new stream. */
  reset(): void {
    this.buffer = "";
  }
}
