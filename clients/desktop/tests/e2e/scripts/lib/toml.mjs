// SPDX-License-Identifier: AGPL-3.0-or-later
// Minimal strict TOML-subset parser for the harness's persona bank. The e2e
// scripts are dependency-free node (no package.json here), so this covers
// exactly what personas.toml uses and FAILS LOUDLY on anything else:
//   - comments (#), blank lines
//   - [[array-of-tables]] headers
//   - key = "basic string" | """multi-line basic string""" | number |
//           true/false | [ single-line array of numbers/strings ]
// Not supported (throws): nested tables, dotted keys, dates, inline tables.
export function parseToml(src, { file = "toml" } = {}) {
  const root = {};
  let current = root;
  const lines = src.split("\n");
  for (let i = 0; i < lines.length; i++) {
    let line = lines[i].trim();
    if (!line || line.startsWith("#")) continue;
    const arrHeader = line.match(/^\[\[([A-Za-z0-9_-]+)\]\]$/);
    if (arrHeader) {
      const name = arrHeader[1];
      root[name] = root[name] ?? [];
      current = {};
      root[name].push(current);
      continue;
    }
    if (/^\[[^\]]+\]$/.test(line)) {
      throw new Error(`${file}:${i + 1}: plain [table] headers unsupported (use [[array]])`);
    }
    const kv = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.*)$/);
    if (!kv) throw new Error(`${file}:${i + 1}: unparseable line: ${line.slice(0, 60)}`);
    const key = kv[1];
    let raw = kv[2];
    // Multi-line basic string: consume until the closing """.
    if (raw.startsWith('"""')) {
      let body = raw.slice(3);
      if (body.includes('"""')) {
        current[key] = body.slice(0, body.indexOf('"""'));
        continue;
      }
      const parts = [body];
      let closed = false;
      while (++i < lines.length) {
        const l = lines[i];
        const at = l.indexOf('"""');
        if (at >= 0) {
          parts.push(l.slice(0, at));
          closed = true;
          break;
        }
        parts.push(l);
      }
      if (!closed) throw new Error(`${file}: unterminated """ for key ${key}`);
      // TOML: a newline immediately after the opening delimiter is trimmed.
      current[key] = parts.join("\n").replace(/^\n/, "");
      continue;
    }
    current[key] = parseValue(raw, `${file}:${i + 1} (${key})`);
  }
  return root;
}

function parseValue(raw, where) {
  raw = raw.replace(/\s+#.*$/, "").trim(); // trailing comment
  if (raw === "true") return true;
  if (raw === "false") return false;
  if (/^-?\d+(\.\d+)?$/.test(raw)) return Number(raw);
  if (raw.startsWith('"')) {
    const m = raw.match(/^"((?:[^"\\]|\\.)*)"$/);
    if (!m) throw new Error(`${where}: bad string: ${raw.slice(0, 60)}`);
    return m[1].replace(/\\"/g, '"').replace(/\\n/g, "\n").replace(/\\\\/g, "\\");
  }
  if (raw.startsWith("[")) {
    const m = raw.match(/^\[(.*)\]$/);
    if (!m) throw new Error(`${where}: bad single-line array: ${raw.slice(0, 60)}`);
    const inner = m[1].trim();
    if (!inner) return [];
    // split on commas outside quotes
    const items = [];
    let cur = "";
    let inStr = false;
    for (const ch of inner) {
      if (ch === '"') inStr = !inStr;
      if (ch === "," && !inStr) {
        items.push(cur.trim());
        cur = "";
      } else cur += ch;
    }
    if (cur.trim()) items.push(cur.trim());
    return items.map((it) => parseValue(it, where));
  }
  throw new Error(`${where}: unsupported value: ${raw.slice(0, 60)}`);
}
