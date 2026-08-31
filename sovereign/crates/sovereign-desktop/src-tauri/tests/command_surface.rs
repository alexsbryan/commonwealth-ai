// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generates the frontend's command bridge types from the Rust
//! `#[tauri::command]` surface, and gates them against drift.
//!
//! The desktop app talks to its backend through 260 commands addressed
//! BY STRING. Nothing checked those strings: `invoke("send_mesage", …)`
//! compiled, shipped, and failed in front of a user as an unhandled
//! rejection from the Tauri bridge. Same for a misspelled argument key
//! — Tauri rejects the call at runtime with "invalid args", and the
//! button simply does nothing.
//!
//! This test parses every command with `syn` and renders
//! `src/lib/commands.generated.ts`: the name union, and each command's
//! argument keys. `src/lib/invoke.ts` types the bridge against it, so a
//! name or key that does not exist is a `svelte-check` failure — which
//! is already a blocking CI gate — rather than a runtime one.
//!
//! Regenerate after intentionally changing the command surface:
//!
//! ```text
//! UPDATE_DESKTOP_COMMANDS=1 cargo test -p sovereign-desktop --test command_surface
//! ```
//!
//! Dev-only: `syn` is a dev-dependency and the commands carry no
//! codegen derives, so this costs nothing at runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use syn::{FnArg, Item, Pat, ReturnType, Type};

fn tauri_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn generated_ts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/lib/commands.generated.ts")
}

/// Parameter types Tauri INJECTS rather than reading from the JS call.
///
/// A command taking `state: State<'_, Arc<AppState>>` is called from JS
/// with no `state` key at all — emitting one would make every existing
/// call site fail to typecheck against an argument it must not send.
fn is_injected(ty: &Type) -> bool {
    let rendered = quote_type(ty);
    const INJECTED: &[&str] = &[
        "AppHandle",
        "State",
        "Window",
        "WebviewWindow",
        "Channel",
        "Request",
    ];
    INJECTED.iter().any(|marker| {
        // Match the type HEAD only: `tauri::State<'_, Arc<AppState>>`
        // and `State<'_, T>` both qualify; a user type merely
        // MENTIONING one of these in a generic argument does not.
        rendered
            .split(['<', ' '])
            .next()
            .is_some_and(|head| head.rsplit("::").next() == Some(*marker))
    })
}

fn quote_type(ty: &Type) -> String {
    use quote::ToTokens;
    ty.to_token_stream().to_string().replace(' ', "")
}

/// `conversation_id` -> `conversationId`. Tauri's default
/// `rename_all = "camelCase"` for command arguments.
fn camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for ch in snake.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// One argument the JS side is expected to supply.
struct Arg {
    key: String,
    /// `Option<T>` params may be omitted, so they render as `key?:`.
    optional: bool,
}

struct CommandDef {
    name: String,
    args: Vec<Arg>,
}

fn parse_commands() -> Vec<CommandDef> {
    let mut files = Vec::new();
    let mut stack = vec![tauri_src()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();

    let mut out: BTreeMap<String, CommandDef> = BTreeMap::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&text) else {
            // A file this crate compiles must parse; if syn cannot, the
            // surface is silently under-reported.
            panic!("syn could not parse {}", path.display());
        };
        collect(&file.items, &mut out);
    }
    out.into_values().collect()
}

fn collect(items: &[Item], out: &mut BTreeMap<String, CommandDef>) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, out);
                }
            }
            Item::Fn(f) => {
                let is_command = f.attrs.iter().any(|a| {
                    let p = a.path();
                    p.segments.last().is_some_and(|s| s.ident == "command")
                        && p.segments.iter().any(|s| s.ident == "tauri" || s.ident == "command")
                });
                if !is_command {
                    continue;
                }
                let name = f.sig.ident.to_string();
                let mut args = Vec::new();
                for input in &f.sig.inputs {
                    let FnArg::Typed(pt) = input else { continue };
                    if is_injected(&pt.ty) {
                        continue;
                    }
                    let Pat::Ident(ident) = pt.pat.as_ref() else {
                        continue;
                    };
                    let raw = ident.ident.to_string();
                    // `_state`-style throwaways are still injected.
                    if raw.starts_with('_') {
                        continue;
                    }
                    args.push(Arg {
                        key: camel(&raw),
                        optional: quote_type(&pt.ty).starts_with("Option<"),
                    });
                }
                out.insert(name.clone(), CommandDef { name, args });
            }
            _ => {}
        }
    }
}

fn render(commands: &[CommandDef]) -> String {
    let mut s = String::new();
    s.push_str(
        "// SPDX-License-Identifier: AGPL-3.0-or-later\n\
         // @generated by `cargo test -p sovereign-desktop --test command_surface`.\n\
         // DO NOT EDIT BY HAND. Regenerate after changing the `#[tauri::command]`\n\
         // surface:\n\
         //\n\
         //   UPDATE_DESKTOP_COMMANDS=1 cargo test -p sovereign-desktop --test command_surface\n\
         //\n\
         // Argument VALUES are `unknown` on purpose: this pins the command names and\n\
         // the argument KEYS, which is what the string-addressed bridge gets wrong.\n\
         // Value types would need every DTO mirrored in TS and are a separate job.\n\n",
    );
    s.push_str("export interface CommandArgs {\n");
    for c in commands {
        if c.args.is_empty() {
            s.push_str(&format!("  {}: Record<string, never>;\n", c.name));
        } else {
            s.push_str(&format!("  {}: {{\n", c.name));
            for a in &c.args {
                s.push_str(&format!(
                    "    {}{}: unknown;\n",
                    a.key,
                    if a.optional { "?" } else { "" }
                ));
            }
            s.push_str("  };\n");
        }
    }
    s.push_str("}\n\nexport type CommandName = keyof CommandArgs;\n");
    s
}

#[test]
fn the_generated_command_bridge_matches_the_rust_surface() {
    let commands = parse_commands();

    // A generator that found nothing renders an empty, permissive map
    // and every call site typechecks against it (ARCH §18.1).
    assert!(
        commands.len() >= 250,
        "parsed only {} #[tauri::command] functions; the surface is ~260, so the \
         generated bridge would be missing commands and `invoke` would reject calls \
         that are actually valid",
        commands.len()
    );

    let rendered = render(&commands);
    let path = generated_ts();

    if std::env::var("UPDATE_DESKTOP_COMMANDS").is_ok() {
        std::fs::write(&path, &rendered).expect("write commands.generated.ts");
        eprintln!("wrote {} ({} commands)", path.display(), commands.len());
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}). Generate it:\n  \
             UPDATE_DESKTOP_COMMANDS=1 cargo test -p sovereign-desktop --test command_surface",
            path.display()
        )
    });

    assert_eq!(
        committed, rendered,
        "the committed command bridge no longer matches the Rust command surface. \
         A command was added, removed, renamed, or had an argument changed, and the \
         frontend's types still describe the old shape. Regenerate:\n  \
         UPDATE_DESKTOP_COMMANDS=1 cargo test -p sovereign-desktop --test command_surface"
    );
}

/// Every command name the frontend actually invokes is a command the
/// backend registers.
///
/// The generated types make this a `svelte-check` failure going
/// forward, but that gate only runs where node is installed. This one
/// runs in the Rust workspace test job and needs neither node nor a
/// build, so a renamed command cannot reach a user through a machine
/// that skipped the frontend checks.
#[test]
fn every_invoked_command_name_is_registered() {
    let registered: std::collections::BTreeSet<String> =
        parse_commands().into_iter().map(|c| c.name).collect();

    let frontend = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src");
    let mut stack = vec![frontend];
    let mut unknown: Vec<String> = Vec::new();
    let mut seen = 0usize;

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "node_modules") {
                    continue;
                }
                stack.push(p);
                continue;
            }
            let is_frontend = p
                .extension()
                .is_some_and(|x| x == "ts" || x == "svelte" || x == "js");
            if !is_frontend {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            for name in invoked_names(&text) {
                seen += 1;
                if !registered.contains(&name) {
                    unknown.push(format!("{}: invoke(\"{name}\")", p.display()));
                }
            }
        }
    }

    assert!(
        seen >= 200,
        "found only {seen} invoke() call sites; the frontend has ~230, so the scanner \
         is reading the wrong tree and this check proves nothing (ARCH §18.1)"
    );
    assert!(
        unknown.is_empty(),
        "the frontend invokes command names the backend does not register — each is a \
         runtime failure a user sees as a control that does nothing:\n  {}",
        unknown.join("\n  ")
    );
}

/// Extracts `NAME` from `invoke("NAME"` / `invoke<T>("NAME"` /
/// `invokeChecked("NAME"`, tolerating whitespace and all three quote
/// styles.
fn invoked_names(text: &str) -> Vec<String> {
    let text = &strip_comments(text);
    let mut out = Vec::new();
    for (idx, _) in text.match_indices("invoke") {
        let rest = &text[idx + "invoke".len()..];
        // Optional `Checked`, then an optional `<...>` type argument.
        let rest = rest.strip_prefix("Checked").unwrap_or(rest);
        let rest = match rest.strip_prefix('<') {
            Some(after) => match after.find('>') {
                Some(close) => &after[close + 1..],
                None => continue,
            },
            None => rest,
        };
        let Some(rest) = rest.strip_prefix('(') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|c| "\"'`".contains(*c)) else {
            continue;
        };
        let after_quote = &rest[quote.len_utf8()..];
        let Some(end) = after_quote.find(quote) else {
            continue;
        };
        let name = &after_quote[..end];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.push(name.to_string());
        }
    }
    out
}

/// Blanks out `//` and `/* */` comments so the scanners read code
/// rather than prose. Quote-aware, so a `"https://..."` literal is not
/// mistaken for a line comment.
///
/// Written because the first run of `every_invoked_command_name_is_registered`
/// flagged `invoke("send_mesage")` — the deliberate typo in `invoke.ts`'s
/// own doc comment explaining what the gate prevents.
fn strip_comments(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            quote = Some(c);
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

// ── falsifiers ────────────────────────────────────────────────────

/// The name scanner must find a name that is not registered. A scanner
/// that silently matched nothing would keep the check green through
/// exactly the rename it exists to catch.
#[test]
fn the_scanner_reports_an_unregistered_name() {
    let found = invoked_names(r#"await invoke("definitely_not_a_command", { a: 1 });"#);
    assert_eq!(found, vec!["definitely_not_a_command".to_string()]);

    // And it reads the real shapes the codebase uses, not just the
    // simplest one.
    assert_eq!(
        invoked_names(r#"invoke<AtomCard | null>("read_get_atom_card", {})"#),
        vec!["read_get_atom_card".to_string()]
    );
    assert_eq!(
        invoked_names(r#"invokeChecked("search_web", { query })"#),
        vec!["search_web".to_string()]
    );
}

/// The injected-parameter filter must drop what Tauri supplies and keep
/// what JS sends. Getting this backwards is silent: every call site
/// would be asked for a `state` key it must never send, or a real
/// argument would vanish from the type and stop being checked.
#[test]
fn injected_parameters_are_excluded_and_real_ones_are_not() {
    let injected: syn::Type = syn::parse_str("State<'_, Arc<AppState>>").unwrap();
    assert!(is_injected(&injected));
    let qualified: syn::Type = syn::parse_str("tauri::State<'_, Arc<AppState>>").unwrap();
    assert!(is_injected(&qualified));
    let handle: syn::Type = syn::parse_str("tauri::AppHandle").unwrap();
    assert!(is_injected(&handle));

    let real: syn::Type = syn::parse_str("String").unwrap();
    assert!(!is_injected(&real));
    let optional: syn::Type = syn::parse_str("Option<Vec<AttachedFile>>").unwrap();
    assert!(!is_injected(&optional));
    // A user type that merely MENTIONS an injected name in a generic
    // argument is still a real argument.
    let mentions: syn::Type = syn::parse_str("Vec<StateSummary>").unwrap();
    assert!(!is_injected(&mentions));
}

#[test]
fn camel_case_matches_tauris_argument_convention() {
    assert_eq!(camel("conversation_id"), "conversationId");
    assert_eq!(camel("message"), "message");
    assert_eq!(camel("surface_skill_id"), "surfaceSkillId");
}

/// `src/lib/invoke.ts` is the only file that may import Tauri's own
/// `invoke`.
///
/// The generated types are worth nothing if a new component imports
/// `@tauri-apps/api/core` directly — that call is back to `cmd: string`
/// and nothing says so at review time. This is what makes the bridge
/// structural rather than a convention: the wrong thing is not
/// discouraged, it fails a test.
#[test]
fn the_invoke_bridge_is_the_only_tauri_core_importer() {
    let frontend = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src");
    const NEEDLE: &str = "@tauri-apps/api/core";
    const BRIDGE: &str = "lib/invoke.ts";

    let mut offenders = Vec::new();
    let mut bridge_seen = false;
    let mut scanned = 0usize;
    let mut stack = vec![frontend];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "node_modules") {
                    continue;
                }
                stack.push(p);
                continue;
            }
            if !p
                .extension()
                .is_some_and(|x| x == "ts" || x == "svelte" || x == "js")
            {
                continue;
            }
            scanned += 1;
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            if !text.contains(NEEDLE) {
                continue;
            }
            let path = p.to_string_lossy().replace('\\', "/");
            // `test-setup.ts` mocks `@tauri-apps/api/core` for vitest; it
            // has to name the real module to intercept it, and it defines
            // no call sites of its own.
            if path.ends_with("src/test-setup.ts") {
                continue;
            }
            if path.ends_with(BRIDGE) {
                bridge_seen = true;
            } else {
                offenders.push(path);
            }
        }
    }

    assert!(
        scanned >= 200,
        "scanned only {scanned} frontend files; the app has ~675, so this check is \
         reading the wrong tree (ARCH §18.1)"
    );
    assert!(
        bridge_seen,
        "no file imports {NEEDLE} at all — {BRIDGE} is supposed to. Either the bridge \
         moved and this test now guards nothing, or the frontend stopped using Tauri."
    );
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "these files reach Tauri's untyped `invoke` directly, bypassing the generated \
         command types:\n  {}\nImport `invoke` from `lib/invoke.ts` instead (or \
         `invokePlugin` for a `plugin:name|method` call).",
        offenders.join("\n  ")
    );
}
