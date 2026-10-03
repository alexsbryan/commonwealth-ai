// Pure incremental SSE parser (INLINE_COMPLETION.md, extension plan).
// One instance per response; feed() raw text chunks as they arrive,
// get back zero or more complete events. Handles the wire quirks that
// matter: lines split across chunks, `data:` continuation lines,
// comment lines (": keep-alive"), and the terminal `data: [DONE]`.

export interface SseEvent {
  data: string;
}

export class SseParser {
  private buf = "";
  private dataLines: string[] = [];

  /** Feed one chunk of response text; returns any events completed by it. */
  feed(chunk: string): SseEvent[] {
    this.buf += chunk;
    const events: SseEvent[] = [];
    let idx: number;
    // Process complete lines only; the remainder stays buffered.
    while ((idx = this.buf.indexOf("\n")) >= 0) {
      // Tolerate CRLF.
      const line = this.buf.slice(0, idx).replace(/\r$/, "");
      this.buf = this.buf.slice(idx + 1);
      const ev = this.line(line);
      if (ev) events.push(ev);
    }
    return events;
  }

  /** End-of-stream: parse any unterminated final line, then emit the pending event. */
  end(): SseEvent[] {
    const events: SseEvent[] = [];
    if (this.buf.length > 0) {
      const rest = this.buf.replace(/\r$/, "");
      this.buf = "";
      const ev = this.line(rest);
      if (ev) events.push(ev);
    }
    const ev = this.flush();
    if (ev) events.push(ev);
    return events;
  }

  /** One raw line (no terminator). Returns an event when the line is a boundary. */
  private line(line: string): SseEvent | null {
    if (line === "") {
      // Blank line = event boundary.
      return this.flush();
    }
    if (line.startsWith(":")) {
      // Comment (keep-alive) — not an event.
      return null;
    }
    if (line.startsWith("data:")) {
      this.dataLines.push(line.slice(5).replace(/^ /, ""));
    }
    // event:/id:/retry: fields are unused on our wire — ignored.
    return null;
  }

  private flush(): SseEvent | null {
    if (this.dataLines.length === 0) return null;
    const data = this.dataLines.join("\n");
    this.dataLines = [];
    return { data };
  }
}
