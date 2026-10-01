// SPDX-License-Identifier: AGPL-3.0-or-later
//! The on-prem kit's route and literal lists agree (phase-b-86, -87).
//!
//! - sovereign/deploy/onprem/nginx/firm-rag.conf proxies exactly the routes
//!   acceptance.sh exercises (its `CLIENT_ROUTES`, every one probed by check
//!   0b), method for method, with nothing more;
//! - each of them is a route a lawyer's key may reach — sovereign-daemon's
//!   `api_keys::KEY_SCOPE`, or a path in `client_auth::AUTH_EXEMPT_PATHS` —
//!   so nginx never fronts a route the daemon refuses the key nginx forwards;
//! - package.sh's hardening gate refuses the same literals
//!   sovereign-onprem's sealed composition e2e watches absent on-prem and
//!   present in stock.
//!
//! It lives here, in no package, because it reads a deploy tree and three
//! crates: in sovereign-onprem it would climb out of the crate root, which
//! boundary-gate refuses of a test a lifted package carries.

use std::collections::BTreeSet;

#[path = "shared/repo_root.rs"]
mod repo_root;
use repo_root::repo_root;

const KIT: &str = "sovereign/deploy/onprem";
const API_KEYS: &str = "sovereign/crates/sovereign-daemon/src/api_keys.rs";
const CLIENT_AUTH: &str = "sovereign/crates/sovereign-daemon/src/client_auth.rs";
const SEALED_E2E: &str = "sovereign/crates/sovereign-onprem/tests/sealed_composition_e2e.rs";

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// `(METHOD, path)` with `{}` for one path segment.
type Route = (String, String);

/// The locations nginx proxies, each with the methods its `limit_except`
/// admits. A proxied location without `limit_except` takes every method,
/// which the kit forbids: it would let a write through a read route.
fn nginx_allowlist(conf: &str) -> BTreeSet<Route> {
    let mut routes = BTreeSet::new();
    let mut block: Option<(String, Vec<String>, bool)> = None;
    for line in conf.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("location ") {
            if line.ends_with('}') {
                // A one-line location (`{ return 404; }`) proxies nothing.
                assert!(!line.contains("proxy_pass"), "{line}");
                continue;
            }
            let rest = rest.trim_end_matches('{').trim();
            let path = if let Some(exact) = rest.strip_prefix("= ") {
                exact.trim().to_string()
            } else if let Some(re) = rest.strip_prefix("~ ") {
                let re = re.trim();
                let body = re
                    .strip_prefix('^')
                    .and_then(|r| r.strip_suffix('$'))
                    .unwrap_or_else(|| panic!("an allow pattern must be anchored: {re}"));
                let path = body.replace("[^/]+", "{}");
                assert!(
                    !path.contains(['[', '(', '*', '+', '?', '|', '\\']),
                    "an allow pattern may use only `[^/]+` segments: {re}"
                );
                path
            } else {
                rest.trim_start_matches("^~").trim().to_string()
            };
            block = Some((path, Vec::new(), false));
        } else if let Some((_, methods, proxied)) = block.as_mut() {
            if let Some(m) = line.strip_prefix("limit_except ") {
                let m = m.split('{').next().unwrap_or_default();
                methods.extend(m.split_whitespace().map(str::to_string));
            } else if line.starts_with("proxy_pass ") {
                *proxied = true;
            } else if line == "}" {
                let (path, methods, proxied) = block.take().expect("open block");
                if proxied {
                    assert!(
                        !methods.is_empty(),
                        "nginx proxies {path} with no limit_except: every method reaches it"
                    );
                    routes.extend(methods.into_iter().map(|m| (m, path.clone())));
                }
            }
        }
    }
    routes
}

/// The lines of a bash array `NAME=(` … `)`, each unquoted.
fn bash_array<'a>(script: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("{name}=(");
    let start = script
        .find(&open)
        .unwrap_or_else(|| panic!("no `{open}` in the script"));
    let body = &script[start..];
    let body = &body[..body
        .find("\n)")
        .unwrap_or_else(|| panic!("{name} never closes"))];
    body.lines()
        .skip(1)
        .map(|l| l.trim().trim_matches('"'))
        .filter(|l| !l.is_empty())
        .collect()
}

/// acceptance.sh's `CLIENT_ROUTES`: `"<methods> <path>"` per entry.
fn acceptance_routes(script: &str) -> BTreeSet<Route> {
    let mut routes = BTreeSet::new();
    for entry in bash_array(script, "CLIENT_ROUTES") {
        let (methods, path) = entry
            .rsplit_once(' ')
            .unwrap_or_else(|| panic!("not `<methods> <path>`: {entry}"));
        routes.extend(
            methods
                .split_whitespace()
                .map(|m| (m.to_string(), path.to_string())),
        );
    }
    routes
}

/// Every string literal in the initializer of the `const` named `name`.
fn const_strings(source: &str, name: &str) -> Vec<String> {
    fn strings(expr: &syn::Expr, out: &mut Vec<String>) {
        match expr {
            syn::Expr::Reference(r) => strings(&r.expr, out),
            syn::Expr::Paren(p) => strings(&p.expr, out),
            syn::Expr::Array(a) => a.elems.iter().for_each(|e| strings(e, out)),
            syn::Expr::Tuple(t) => t.elems.iter().for_each(|e| strings(e, out)),
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => out.push(s.value()),
            other => panic!(
                "a const of string literals only, got {}",
                quote::quote!(#other)
            ),
        }
    }
    let file = syn::parse_file(source).expect("the source parses");
    let item = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Const(c) if c.ident == name => Some(c),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no `const {name}`"));
    let mut out = Vec::new();
    strings(&item.expr, &mut out);
    out
}

#[test]
fn nginx_proxies_exactly_the_routes_acceptance_exercises() {
    let nginx = nginx_allowlist(&read(&format!("{KIT}/nginx/firm-rag.conf")));
    let acceptance = acceptance_routes(&read(&format!("{KIT}/acceptance.sh")));
    assert!(
        !nginx.is_empty(),
        "nginx proxies nothing: the parser read no location"
    );
    let only_nginx: Vec<_> = nginx.difference(&acceptance).collect();
    let only_acceptance: Vec<_> = acceptance.difference(&nginx).collect();
    assert!(
        only_nginx.is_empty() && only_acceptance.is_empty(),
        "nginx/firm-rag.conf and acceptance.sh's CLIENT_ROUTES disagree.\n  \
         proxied but never exercised: {only_nginx:?}\n  \
         exercised but not proxied: {only_acceptance:?}"
    );
}

#[test]
fn every_proxied_route_is_one_a_lawyer_key_may_reach() {
    let nginx = nginx_allowlist(&read(&format!("{KIT}/nginx/firm-rag.conf")));
    let scope = const_strings(&read(API_KEYS), "KEY_SCOPE");
    assert!(
        scope.len() % 2 == 0 && !scope.is_empty(),
        "KEY_SCOPE reads as (method, path) pairs: {scope:?}"
    );
    let scope: Vec<(&str, &str)> = scope
        .chunks(2)
        .map(|p| (p[0].as_str(), p[1].as_str()))
        .collect();
    let exempt = const_strings(&read(CLIENT_AUTH), "AUTH_EXEMPT_PATHS");
    assert!(!exempt.is_empty(), "read no AUTH_EXEMPT_PATHS");
    for (method, path) in &nginx {
        let in_scope = scope
            .iter()
            .any(|(m, p)| (*m == "*" || m == method) && p == path);
        assert!(
            in_scope || exempt.iter().any(|e| e == path),
            "nginx proxies {method} {path}, which the daemon refuses a lawyer's key \
             (not in sovereign-daemon api_keys::KEY_SCOPE)"
        );
    }
}

#[test]
fn package_refuses_the_literals_the_sealed_e2e_watches() {
    let package = read(&format!("{KIT}/package.sh"));
    let gate: BTreeSet<String> = bash_array(&package, "NOT_COMPOSED")
        .into_iter()
        .map(str::to_string)
        .collect();
    let watched: BTreeSet<String> = const_strings(&read(SEALED_E2E), "NOT_COMPOSED")
        .into_iter()
        .collect();
    assert!(!watched.is_empty(), "read no literal from the e2e");
    assert_eq!(
        gate, watched,
        "package.sh's strings gate and sealed_composition_e2e's NOT_COMPOSED differ"
    );
}

/// The parser is the instrument: it must see a proxied route that is not in
/// the list, or the equality above could be vacuous.
#[test]
fn the_nginx_parser_sees_a_proxied_location() {
    let conf = "    location ~ ^/v1/x/[^/]+$ {\n        limit_except GET POST { deny all; }\n        proxy_pass http://u;\n    }\n    location = /y { return 404; }\n";
    let routes = nginx_allowlist(conf);
    let expected: BTreeSet<Route> = [("GET", "/v1/x/{}"), ("POST", "/v1/x/{}")]
        .into_iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect();
    assert_eq!(routes, expected);
    assert!(repo_root().join(KIT).is_dir());
}
