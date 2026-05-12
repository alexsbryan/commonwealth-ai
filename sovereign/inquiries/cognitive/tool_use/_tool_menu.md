Available tools — pick exactly one (or "none") per task.

CODE INTELLIGENCE (Sovereign MCP)
- `symbols(name="<TypeOrFn>")` — return the exact definition site of a symbol with file:line.
- `callers(name="<fn>")` — every call site of a function (compiler-resolved, includes trait dispatch).
- `callees(name="<fn>")` — every function this function calls.
- `code_search(query="<freeform>")` — semantic + lexical search across the indexed corpus when you don't have an exact symbol name.
- `blast(name="<symbol>", max_depth=2)` — transitive blast radius of a symbol; how many things depend on it.
- `notes(query="<topic>")` — durable decisions/invariants/attempts from prior sessions.

DRIFT & DOCS
- `drift_findings(query="<symbol or claim>")` — narrative-doc drift findings touching a symbol.

FILESYSTEM & SHELL (Claude Code core)
- `Read(file_path)` — read a file; use when the path is known.
- `Edit(file_path, old_string, new_string)` — make a targeted edit at a known site.
- `Glob(pattern)` — list files matching a glob (e.g., `**/*.rs`).
- `Grep(pattern, path)` — regex search file contents.
- `Bash(command)` — shell command (build, test, query daemon, etc.).

NO-OP
- `none` — the task is answerable from the prompt alone (or with conversation context). Do NOT call a tool.

OUTPUT FORMATS
- Single call:     {"tool": "<name>", "args": {...}}
- No tool needed:  {"tool": "none", "rationale": "<one sentence>"}
- Ordered chain:   {"tools": ["<name>", "<name>", ...]}

Output exactly one JSON object. No prose around it. No markdown fences.
