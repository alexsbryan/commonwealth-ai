// SPDX-License-Identifier: AGPL-3.0-or-later
//! TOML parsing + parameter validation + serde error rewriting —
//! extracted out of `crate::recipe`.
//!
//! Pure helper free fns. `Recipe::from_toml` and
//! `Recipe::resolve_parameters` (still in `recipe.rs`) call into here.
//! Behaviour-preserving — same diagnostics, same arms, same wording.

use crate::error::{Error, Result};
use crate::recipe::{ParameterKind, ParameterValue, Recipe, MAX_SCHEMA_VERSION};

pub(crate) fn empty_value(kind: &ParameterKind) -> ParameterValue {
    match kind {
        ParameterKind::String | ParameterKind::Date => ParameterValue::String(String::new()),
        ParameterKind::Int => ParameterValue::Int(0),
        ParameterKind::List => ParameterValue::List(Vec::new()),
    }
}

pub(crate) fn parameter_value_from_toml(
    name: &str,
    kind: &ParameterKind,
    v: toml::Value,
) -> Result<ParameterValue> {
    match (kind, v) {
        (ParameterKind::String, toml::Value::String(s)) => Ok(ParameterValue::String(s)),
        (ParameterKind::Int, toml::Value::Integer(i)) => Ok(ParameterValue::Int(i)),
        (ParameterKind::Int, toml::Value::String(s)) => {
            s.parse::<i64>().map(ParameterValue::Int).map_err(|e| {
                Error::InvalidInput(format!("parameter `{name}` is not an integer: {s} ({e})"))
            })
        }
        (ParameterKind::Date, toml::Value::String(s)) => {
            if !is_iso_date(&s) {
                return Err(Error::InvalidInput(format!(
                    "parameter `{name}` is not an ISO-8601 date (YYYY-MM-DD): {s}"
                )));
            }
            Ok(ParameterValue::Date(s))
        }
        (ParameterKind::List, toml::Value::Array(arr)) => {
            let mut items = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    toml::Value::String(s) => items.push(s),
                    other => {
                        return Err(Error::InvalidInput(format!(
                            "parameter `{name}` list entries must be strings, got: {other:?}"
                        )))
                    }
                }
            }
            Ok(ParameterValue::List(items))
        }
        // Convenience: comma-separated string for list parameters.
        // The CLI prompt yields one string; the desktop form yields
        // a true array. Both should work.
        (ParameterKind::List, toml::Value::String(s)) => {
            let items = s
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
            Ok(ParameterValue::List(items))
        }
        (kind, other) => Err(Error::InvalidInput(format!(
            "parameter `{name}` expected {kind:?}, got TOML value: {other:?}"
        ))),
    }
}

/// Refuse recipes whose declared `schema_version` is higher than
/// the engine knows. See [`MAX_SCHEMA_VERSION`].
pub(crate) fn check_schema_version(v: u32) -> Result<()> {
    if v > MAX_SCHEMA_VERSION {
        return Err(Error::Recipe(format!(
            "recipe declares schema_version = {v} but this engine \
             supports schema_version <= {MAX_SCHEMA_VERSION}. \
             The recipe was authored against a newer engine; \
             upgrade `corpus-engine` to load it."
        )));
    }
    Ok(())
}

/// The three ontology-version rules, at the load boundary
/// (`ONTOLOGY_MIGRATION.md` §0; ARCH §18.3 never silently substitute):
///
/// 1. **Unknown version refuses naming the max** — `OntologyBlock::language`.
/// 2. **A later version's key without its version line refuses naming the
///    fix.** Serde would otherwise drop `types = […]` from a version-0 block
///    without a sound, and the author would enrich a corpus that hears none of
///    their declarations. The key→version map is the registry's own key lists
///    (`first_version_defining`), never a second list here.
/// 3. **Structural errors surface at load** — the block is parsed eagerly
///    through its language, so a claim type without `force` or an unknown
///    `kind` fails `Recipe::from_toml`, not `enrich extract`.
///
/// Keys NO version defines are a `validate` warning, not a load error:
/// community recipes must keep loading (`deny_unknown_fields` was rejected
/// for that reason), and a typo cannot change what the pipeline does.
pub(crate) fn check_ontology_block(recipe: &Recipe) -> Result<()> {
    let Some(block) = recipe.enrichment.as_ref().and_then(|e| e.ontology.as_ref()) else {
        return Ok(());
    };
    let lang = block.language()?;
    let registry = crate::recipe_ontology::language::OntologyLanguageRegistry::builtin();
    let mut later: Vec<(u32, &String)> = block
        .body
        .keys()
        .filter(|k| !lang.keys().contains(&k.as_str()))
        .filter_map(|k| {
            registry
                .first_version_defining(k)
                .filter(|v| *v > block.version)
                .map(|v| (v, k))
        })
        .collect();
    later.sort();
    if let Some((needed, key)) = later.first() {
        let absent = if block.version == 0 {
            " (no `version` line, so version 0)"
        } else {
            ""
        };
        return Err(Error::Recipe(format!(
            "[enrichment.ontology] uses `{key}`, which belongs to ontology version {needed}, \
             but the block declares version {}{absent}. Add `version = {needed}` directly \
             under `[enrichment.ontology]`, or run `svrn recipe migrate --ontology-version \
             {needed} <recipe.toml>`. Version {} accepts: {}.",
            block.version,
            block.version,
            lang.keys().join(", ")
        )));
    }
    lang.parse(&block.body)?;
    Ok(())
}

/// Text-level migration behind `Recipe::migrate_ontology_version`. Works on
/// the source text (a recipe that NEEDS migrating fails `from_toml` by rule 2
/// above, so it cannot be parsed first): find the `[enrichment.ontology]`
/// header, then within its table either replace an existing `version = N`
/// line or insert one directly under the header. The result is verified
/// through `Recipe::from_toml` before it is returned.
pub(crate) fn migrate_ontology_version(toml_str: &str, target: u32) -> Result<Option<String>> {
    let lines: Vec<&str> = toml_str.lines().collect();
    let is_header = |l: &str| {
        let t = l.trim();
        t.starts_with('[') && !t.starts_with("[[")
    };
    let header_idx = lines
        .iter()
        .position(|l| {
            let t = l.trim();
            let t = t.split('#').next().unwrap_or("").trim();
            t == "[enrichment.ontology]"
        })
        .ok_or_else(|| {
            Error::Recipe(
                "recipe has no `[enrichment.ontology]` table header to attach `version` to \
                 (dotted-key or inline forms are not rewritten; add the line by hand)"
                    .to_string(),
            )
        })?;
    // The block's own scalar lines run until the next table header of any kind.
    let block_end = lines[header_idx + 1..]
        .iter()
        .position(|l| is_header(l) || l.trim().starts_with("[["))
        .map(|i| header_idx + 1 + i)
        .unwrap_or(lines.len());
    let version_line = (header_idx + 1..block_end).find(|&i| {
        let t = lines[i].trim();
        t.starts_with("version") && t[7..].trim_start().starts_with('=')
    });
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match version_line {
        Some(i) => {
            let current: u32 = lines[i]
                .split('=')
                .nth(1)
                .and_then(|v| v.split('#').next())
                .and_then(|v| v.trim().parse().ok())
                .ok_or_else(|| {
                    Error::Recipe(format!(
                        "could not read the existing ontology version from line {}: {}",
                        i + 1,
                        lines[i]
                    ))
                })?;
            if current >= target {
                return Ok(None);
            }
            out[i] = format!("version = {target}");
        }
        None => out.insert(header_idx + 1, format!("version = {target}")),
    }
    let mut migrated = out.join("\n");
    if toml_str.ends_with('\n') {
        migrated.push('\n');
    }
    Recipe::from_toml(&migrated).map_err(|e| {
        Error::Recipe(format!(
            "adding `version = {target}` does not yield a loadable recipe — fix the block \
             first: {e}"
        ))
    })?;
    Ok(Some(migrated))
}

/// Refuse a `field_model` recipe whose `[enrichment] domain` names
/// something the field-model domain registry does not carry.
///
/// **Why this is a load-time check and not a runtime one.** The runtime
/// check already exists — `FieldModelEngine::from_recipe`
/// (`enrichment/field_engine.rs:74`) raises
/// [`Error::UnknownEnrichmentDomain`]. But it fires *after* acquire,
/// extract, embed and index have all run: the corpus is fully built,
/// then the install fails and strands a partition. Observed twice on
/// 2026-08-07 (`brothers_karamazov` 17:38:45Z, `brothers-karamazov-book-1`
/// 19:07:30Z, both `Unknown enrichment domain: literary`) — the recipe
/// carried `type = "field_model"` with `domain = "literary"`, which names
/// an atlas *pipeline* (`literary_atlas`), not a field-model *domain*.
/// Two registries, one word. Checking it here costs a string compare and
/// moves the failure from "after the expensive part" to "before it".
///
/// **Three conditions, all required, and each is load-bearing:**
///
/// 1. `enabled` — a disabled `[enrichment]` block never constructs the
///    engine (`engine/ingest.rs`'s `'enrichment:` block is inside the
///    `enabled` guard), so a bad domain there is inert. Rejecting it
///    would fail a recipe that has no failing *run*, which is the
///    inverse of ARCH_PRINCIPLES §18.1.
/// 2. `type == "field_model"` — `atlas`, `investigation` and `tiered`
///    recipes take `break 'enrichment` early-outs (`engine/ingest.rs`
///    ~1786 / ~1808) and never reach the domain registry. Their `domain`
///    selects from a *different* registry and must not be judged here.
/// 3. `domain` is `Some` — `from_recipe` falls back to `"philosophy"`
///    when it is absent, and `"philosophy"` is registered. Absent is a
///    real, working configuration; only a present-and-wrong domain is
///    the failure this gate names.
///
/// The valid set is read from [`DomainRegistry::builtin`] itself, never
/// re-listed here — one decider (§10.6). Registering a sixth domain
/// widens this gate in the same commit, with no second edit.
///
/// [`DomainRegistry::builtin`]: crate::enrichment::domain_registry::DomainRegistry::builtin
/// Refuse a recipe whose `[enrichment] type` names no registered pass —
/// at load, with the valid set listed (ARCH §4.3).
///
/// Before this gate an unknown type was five different things at once:
/// `engine/ingest.rs` ran `field_model` for it, the `enrichment_requested`
/// stamp said "expected", `enrichment_drift` said "unverifiable", the
/// desktop's "enrich now" ran `tiered`, and boot-resume said "not
/// resumable". `type = "foo"` therefore installed a field model and was
/// structurally invisible to every health check. Now it does not load.
///
/// Checked whether or not `enabled` is set: a typo in a disabled block is
/// still a typo, and the author flipping `enabled = true` later should not
/// be the first time they hear about it. The valid set is read from
/// [`EnrichmentPassRegistry::builtin`], never re-listed here (§10.6).
///
/// [`EnrichmentPassRegistry::builtin`]: crate::enrichment::pass::EnrichmentPassRegistry::builtin
pub(crate) fn check_enrichment_type(recipe: &Recipe) -> Result<()> {
    let Some(enrichment) = recipe.enrichment.as_ref() else {
        return Ok(());
    };
    crate::enrichment::pass::EnrichmentPassRegistry::builtin()
        .resolve(&enrichment.enrichment_type)
        .map(|_| ())
        .map_err(|e| {
            Error::Recipe(format!(
                "recipe `{corpus_id}` declares [enrichment] {e}",
                corpus_id = recipe.corpus.id,
            ))
        })
}

pub(crate) fn check_enrichment_domain(recipe: &Recipe) -> Result<()> {
    let Some(enrichment) = recipe.enrichment.as_ref() else {
        return Ok(());
    };
    if !enrichment.enabled || enrichment.enrichment_type != crate::enrichment::pass::FIELD_MODEL {
        return Ok(());
    }
    let Some(domain) = enrichment.domain.as_deref() else {
        return Ok(());
    };

    let registry = crate::enrichment::domain_registry::DomainRegistry::builtin();
    if registry.get(domain).is_some() {
        return Ok(());
    }

    // Absence is reported, never defaulted (§18.3): name the domain we
    // were handed AND the complete set that would have worked, so the
    // author never has to go find the registry to learn what to type.
    let mut valid = registry.domain_ids();
    valid.sort_unstable();
    Err(Error::Recipe(format!(
        "recipe `{corpus_id}` declares [enrichment] type = \"field_model\" \
         with domain = \"{domain}\", which is not a registered field-model \
         domain. Valid field-model domains are: {valid}. \
         If \"{domain}\" is an atlas pipeline (e.g. `literary`, \
         `philosophy`, `referential`), the recipe wants \
         type = \"atlas\" — atlas pipelines and field-model domains are \
         separate registries that happen to share a key name.",
        corpus_id = recipe.corpus.id,
        valid = valid.join(", "),
    )))
}

/// Translate a serde TOML parse error into something actionable
/// for the recipe author. Three rewrite passes, in order:
///
/// 1. **Deprecation aliases** (e.g. `api_paginated` → `http_api`):
///    name the replacement so the user doesn't reverse-engineer
///    the rename from a generic "unknown variant" message.
/// 2. **Missing required fields**: rephrase `missing field 'X'`
///    in plain language and, when the field is a section we know
///    well, list valid `type` values inline. The default serde
///    message points the caret at line 1 even when the issue is
///    "the section doesn't exist anywhere" — that's misleading and
///    the rewrite drops it.
/// 3. **Unknown enum variants**: name the field path that the
///    bad value was assigned to, when the parse error carries
///    enough position info to recover it. The default message
///    quotes the bad value but not the field, so a recipe with
///    `[acquire.follow] document_format = "pdf"` reads as just
///    "unknown variant 'pdf'" with no field hint.
///
/// Falls through to the raw serde message when no rewrite
/// applies — better to surface the technical error than to
/// invent a "helpful" rewrite that misdescribes the failure.
pub(crate) fn translate_parse_error(e: toml::de::Error) -> Error {
    const DEPRECATIONS: &[(&str, &str, &str)] = &[
        // (deprecated_name, replacement, since)
        (
            "api_paginated",
            "http_api",
            "PR1 — recipe-authoring platform",
        ),
    ];
    let raw = e.to_string();

    // 1. Deprecation aliases — keep first so a deprecated variant
    //    name takes precedence over the generic "unknown variant"
    //    rewrite below.
    for (old, new, since) in DEPRECATIONS {
        if raw.contains(old) {
            return Error::Recipe(format!(
                "recipe references the removed acquirer/extractor type \
                 `{old}`. Migrate to `{new}` (replaced in {since}). \
                 See SYSTEM_OVERVIEW.md §3.10. Underlying parse error: {raw}"
            ));
        }
    }

    // 2. Missing required field — `missing field \`X\`` (single
    //    backticks in serde's output).
    if let Some(field) = extract_missing_field(&raw) {
        return Error::Recipe(rewrite_missing_field(&field, &raw));
    }

    // 3. Unknown variant — `unknown variant \`X\`, expected one of …`
    if let Some((bad_value, allowed)) = extract_unknown_variant(&raw) {
        return Error::Recipe(rewrite_unknown_variant(&bad_value, &allowed, &raw));
    }

    Error::Recipe(raw)
}

/// Pull the field name out of a serde `missing field \`X\`` message.
pub(crate) fn extract_missing_field(raw: &str) -> Option<String> {
    let anchor = "missing field `";
    let start = raw.find(anchor)? + anchor.len();
    let rest = &raw[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Pull `(bad_value, allowed_csv)` out of a serde
/// `unknown variant \`X\`, expected one of \`a\`, \`b\`, …` message.
pub(crate) fn extract_unknown_variant(raw: &str) -> Option<(String, String)> {
    let var_anchor = "unknown variant `";
    let var_start = raw.find(var_anchor)? + var_anchor.len();
    let after_var = &raw[var_start..];
    let var_end = after_var.find('`')?;
    let bad_value = after_var[..var_end].to_string();
    // Allowed list: everything between "expected one of " and the
    // end of the line / next backtick-free run. Serde emits the
    // list with backticks; surface it as plain CSV.
    let allowed_anchor = "expected one of ";
    let allowed_start = raw.find(allowed_anchor)? + allowed_anchor.len();
    let allowed_chunk = &raw[allowed_start..];
    let allowed_end = allowed_chunk.find('\n').unwrap_or(allowed_chunk.len());
    let allowed = allowed_chunk[..allowed_end].replace('`', "");
    Some((bad_value, allowed))
}

/// Compose a plain-language explanation for a missing required key,
/// and inline the valid `type` values when the missing field names
/// a section whose `type` enum we know up-front. The known sections
/// stay narrow on purpose — better to fall back to the raw serde
/// message than to give wrong "valid types" guidance.
fn rewrite_missing_field(field: &str, raw: &str) -> String {
    match field {
        "acquire" => format!(
            "Recipe is missing the `[acquire]` section. Every recipe needs \
             one. Add it with `type = \"...\"` (one of: bulk_download | \
             http_api | web_crawl | local_file | huggingface_dataset). \
             Underlying parser error: {raw}"
        ),
        "extract" => format!(
            "Recipe is missing the `[extract]` section. Add it with \
             `type = \"...\"` (one of: plaintext | html | html_sections | \
             json | jsonl | csv | parquet | mediawiki_xml | \
             stackexchange_xml | wikipedia_jsonl | wikipedia_structured | \
             wikipedia_catalog | wikipedia_api_article | gutenberg_catalog \
             | code | markdown). Underlying parser error: {raw}"
        ),
        "chunk" => format!(
            "Recipe is missing the `[chunk]` section. Add it with \
             `type = \"...\"` (one of: paragraph | sentence | fixed | \
             semantic | passthrough). Underlying parser error: {raw}"
        ),
        "corpus" => format!(
            "Recipe is missing the `[corpus]` section. Every recipe needs \
             one with at least `id = \"...\"` and `name = \"...\"`. \
             Underlying parser error: {raw}"
        ),
        "type" => format!(
            "A section is missing its required `type` field. Look at the \
             TOML caret below to see which section. Each acquirer / \
             extractor / chunker / pattern needs an explicit `type = \
             \"...\"`. Underlying parser error: {raw}"
        ),
        "base_url" => format!(
            "An `[acquire]` block with `type = \"http_api\"` is missing \
             `base_url`. Add `base_url = \"https://api.example.com\"`. \
             Underlying parser error: {raw}"
        ),
        "id" | "name" => format!(
            "The `[corpus]` section is missing required field `{field}`. \
             Both `id` (stable identifier) and `name` (display name) are \
             required. Underlying parser error: {raw}"
        ),
        "document_path" => format!(
            "An `[extract]` block with `type = \"json\"` is missing \
             `document_path`. Set it to a JSONPath that selects the \
             documents array (e.g. `$.results[*]`). Underlying parser \
             error: {raw}"
        ),
        "content_field" => format!(
            "An `[extract]` block is missing `content_field` — the name \
             of the JSON field on each matched object that holds the \
             document body text. Underlying parser error: {raw}"
        ),
        _ => format!(
            "Recipe is missing required field `{field}`. Add it to the \
             section the parser caret points at below. Underlying parser \
             error: {raw}"
        ),
    }
}

/// Compose a plain-language explanation for an unknown enum value,
/// naming the field path when the parse error carries enough span
/// info for us to recover it. Serde's default points the caret at
/// the assignment but doesn't quote the field name in the error
/// text, which makes the message read as just "unknown variant 'X'".
fn rewrite_unknown_variant(bad_value: &str, allowed: &str, raw: &str) -> String {
    let field_hint = extract_field_from_span(raw, bad_value);
    let field_phrase = match field_hint.as_deref() {
        Some(f) => format!("field `{f}`"),
        None => "a field".to_string(),
    };
    format!(
        "{field_phrase} got `{bad_value}` but allowed values are: \
         {allowed}. Underlying parser error: {raw}"
    )
}

/// Best-effort extraction of `key` from a serde TOML error span
/// containing `key = "<bad_value>"` or similar. Walks the message
/// line-by-line looking for an `=` neighbouring the bad value.
fn extract_field_from_span(raw: &str, bad_value: &str) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim_start_matches(|c: char| c.is_ascii_digit() || c == '|' || c == ' ');
        if !trimmed.contains(bad_value) || !trimmed.contains('=') {
            continue;
        }
        let key_part = trimmed.split('=').next()?.trim();
        if !key_part.is_empty()
            && key_part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            return Some(key_part.to_string());
        }
    }
    None
}

/// Lexical ISO-8601 calendar-date check (`YYYY-MM-DD`). We don't
/// validate semantic correctness (e.g. February 30) here — that's
/// the caller's job. This function exists so the recipe schema
/// doesn't gain a dependency on `chrono` purely for parameter
/// validation.
pub(crate) fn is_iso_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|c| c.is_ascii_digit())
        && bytes[5..7].iter().all(|c| c.is_ascii_digit())
        && bytes[8..].iter().all(|c| c.is_ascii_digit())
}
