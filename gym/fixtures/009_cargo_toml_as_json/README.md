# 009 — Cargo.toml emitted as JSON

**What it tests:** the model successfully pivots to apply_patch (per
read-attractor fix) and emits well-structured patch syntax (per
heredoc canonicalizer extensions), but the Cargo.toml CONTENT is
JSON-shaped, not TOML.

**Captured from:** smoke v5 (2026-05-13). After all the protocol-
level fixes landed, the model emitted patches that codex accepted,
but the actual Cargo.toml looked like:

```toml
+{
+    name = "oicp-types",
+    version.workspace = true,
+    edition.workspace = true,
+
+    [dependencies]
+    serde = { version = "1.0", features = ["derive"] }
+}
```

That's TOML key=value syntax wrapped in JSON object braces with
trailing commas. Codex's apply_patch accepts it (it's a valid
heredoc), but `cargo check` rejects it as malformed TOML.

**Why this is a content-quality bug, not a protocol bug:** the
patch structure is valid (Begin Patch, Add File, +-prefixed lines,
End Patch). The model just doesn't remember Cargo.toml syntax under
load — it falls back to JSON-like object shape because that's how
the spec.md describes it visually.

**Pass criteria:**
- args parses as JSON
- `cmd` contains `apply_patch` AND `Cargo.toml` AND a TOML
  `[package]` section header
- `cmd` does NOT contain JSON-style opening `{` immediately after
  the Add File: line
- `cmd` does NOT contain trailing commas on key=value lines

**Empirical baseline (pre-fix):** 0/N — model deterministically emits
the JSON shape on this fixture's context.

**Investments that should move this fixture:**
- Mechanical Cargo.toml content canonicalizer: detect JSON-shape body
  inside an `Add File: *Cargo.toml` section, strip wrapper braces and
  trailing commas, inject `[package]` header if absent
- Same pattern generalizes to any well-known config file format
  (pyproject.toml, package.json, etc.) — fixtures for those when we
  hit them
