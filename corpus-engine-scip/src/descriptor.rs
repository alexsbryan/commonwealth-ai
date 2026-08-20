// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE decider for "what kind of thing is this symbol, and how is it
//! reached?" — derived from the SCIP descriptor, which is 100% populated.
//!
//! ## Why this module exists
//!
//! The `symbols.kind` column is a derived value that drifted from the data it
//! was derived from, and it is now worse than absent. Measured on the
//! `commonwealth-ai` graph, 2026-08-19 (313,741 symbols):
//!
//! | `kind` value | rows | truth |
//! |---|---:|---|
//! | `unknown` | 278,233 (88.7%) | no information |
//! | `enum` | 19,691 | only 1,560 carry a variant descriptor |
//! | `constructor` | 946 | every one is a top-level TYPE descriptor |
//! | `method` | 131 | every one is a top-level TYPE descriptor |
//!
//! Of the 8,759 top-level type descriptors, **not one** is labelled a type:
//! 7,682 say `unknown`, 946 say `constructor`, 131 say `method`. The column's
//! cited failure is real and reproducible — `sovereign_contracts` `Intent`,
//! an enum, is tagged `constructor`.
//!
//! `refs.ref_kind` has the same shape one table over: the `"dynamic"` constant
//! exists in the schema and is never written, so all 1,564,645 edges read
//! `"direct"` and dispatch cannot be filtered. But the graph DOES record trait
//! dispatch — a call through `Arc<dyn InferenceProvider>` at
//! `runtime/streaming.rs:208` lands on `traits/InferenceProvider#…().`, the
//! trait's own declaration, while concrete impls carry the distinct
//! `impl#[Concrete][Trait]…().` shape. The fact was never missing; only the
//! column that was supposed to carry it. So: **derive it, do not maintain it**
//! (`ARCH_PRINCIPLES` §10.6 — one decider, one name).
//!
//! ## Descriptor grammar, as SCIP defines it and as this graph exhibits it
//!
//! ```text
//! types/ScoredChunk#                                    type
//! StartupOutcome#Failed#                                enum variant
//! DepEdge#from.                                         field
//! runtime/streaming/run_synthesis_stream().             free function
//! impl#[Runtime]handle_message().                       inherent method
//! impl#[MeshInferenceProvider][InferenceProvider]f().   trait impl method
//! traits/InferenceProvider#complete().                  trait method decl
//! workflow_cmd/HELP.                                    term (const / static)
//! ids/define_id!                                        macro
//! crate/                                                module
//! local 0                                               local
//! ```

use serde::Serialize;

/// What a SCIP descriptor names. Derived, never stored.
///
/// NOT `SymbolKind` — `corpus_engine::extractors::code::SymbolKind` already
/// owns that name for the tree-sitter extractor's SOURCE-language
/// classification (Struct / Enum / Class / Interface / Impl). This one
/// classifies a SCIP *descriptor*, which draws different lines: SCIP
/// deliberately does not separate struct from enum from trait (they are all
/// `Foo#`), and tree-sitter never emits a trait-method declaration, a
/// parameter binding, or a meta descriptor. Two concepts, one obvious name;
/// disposition `distinct`, recorded here because
/// `svrn code converge status` caught the collision the hour it was
/// introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DescriptorKind {
    /// `Foo#` — struct, enum, trait, type alias. SCIP does not distinguish
    /// them in the descriptor, and neither do we: claiming more than the data
    /// carries is how `kind` got into this state.
    Type,
    /// `Enum#Variant#`
    EnumVariant,
    /// `Type#field.`
    Field,
    /// `path/name().` — free function.
    Function,
    /// `impl#[Type]name().` — inherent method.
    Method,
    /// `impl#[Type][Trait]name().` — a concrete trait implementation.
    TraitImplMethod,
    /// `path/Trait#name().` — the declaration on the trait itself. A call
    /// edge landing here was dispatched through the trait.
    TraitMethod,
    /// `path/NAME.` — const, static, or module-level binding.
    Term,
    /// `name!`
    Macro,
    /// `path/`
    Module,
    /// `impl#[Type]` / `impl#[Type][Trait]` — the impl block itself, not a
    /// member of it.
    ImplBlock,
    /// `fn().(param)` — a parameter binding. Emitted heavily by
    /// `scip-python`; 21,809 rows on this graph.
    Parameter,
    /// `path/name:` — a SCIP meta descriptor (`__init__:` and friends).
    Meta,
    /// `local N` — rust-analyzer's block-scoped locals.
    Local,
    /// The descriptor is present but matches no known shape. Distinct from
    /// "we didn't look": absence is reported, never defaulted (§18.3).
    Unrecognized,
}

impl DescriptorKind {
    /// Types, variants and fields — the things a concept census counts.
    pub fn is_type_like(self) -> bool {
        matches!(self, Self::Type | Self::EnumVariant | Self::Field)
    }

    /// Anything with a body that can call something else.
    pub fn is_callable(self) -> bool {
        matches!(
            self,
            Self::Function | Self::Method | Self::TraitImplMethod | Self::TraitMethod
        )
    }
}

/// How a call edge reached its callee.
///
/// Deliberately NOT named `Static`/`Dynamic`: a call through a generic
/// `impl Trait` bound also lands on the trait's declaration and is
/// monomorphized — statically dispatched. The descriptor cannot separate the
/// two, so this reports a candidate and says so in the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchHint {
    /// The callee is a concrete definition.
    Direct,
    /// The callee is a trait method declaration — `dyn` dispatch or a generic
    /// bound. The boundary a reader must be told about either way.
    ThroughTrait,
}

/// Strip the `<scheme> <manager> <package> <version> ` prefix, leaving the
/// descriptor. Returns the input unchanged when it carries no prefix, so this
/// is safe to call on a bare descriptor.
pub fn descriptor_of(qualified_name: &str) -> &str {
    if qualified_name.starts_with("local ") {
        return qualified_name;
    }
    let mut it = qualified_name.splitn(5, ' ');
    match (it.next(), it.next(), it.next(), it.next(), it.next()) {
        (Some(_), Some(_), Some(_), Some(_), Some(desc)) => desc,
        _ => qualified_name,
    }
}

/// The final `/`-segment of a descriptor with its terminator stripped:
/// `types/ScoredChunk#` -> `ScoredChunk`, `m/run().` -> `run`.
pub fn leaf_name(qualified_name: &str) -> &str {
    let desc = descriptor_of(qualified_name);
    desc.rsplit('/')
        .next()
        .unwrap_or(desc)
        .trim_end_matches(['#', '.', '!', '/'])
}

/// Classify a symbol from its qualified name or bare descriptor.
pub fn descriptor_kind(qualified_name: &str) -> DescriptorKind {
    let desc = descriptor_of(qualified_name);
    if desc.is_empty() {
        return DescriptorKind::Unrecognized;
    }
    if desc.starts_with("local ") {
        return DescriptorKind::Local;
    }
    if desc.ends_with('!') {
        return DescriptorKind::Macro;
    }
    if desc.ends_with(')') {
        return DescriptorKind::Parameter;
    }
    if desc.ends_with(':') {
        return DescriptorKind::Meta;
    }
    if desc.ends_with(']') {
        return DescriptorKind::ImplBlock;
    }
    if desc.ends_with('/') {
        return DescriptorKind::Module;
    }
    if desc.ends_with('#') {
        // One `#` is the type itself; two is a variant nested under its enum.
        return match desc.bytes().filter(|b| *b == b'#').count() {
            1 => DescriptorKind::Type,
            _ => DescriptorKind::EnumVariant,
        };
    }
    if desc.ends_with('.') {
        let leaf = desc.rsplit('/').next().unwrap_or(desc);
        if !leaf.contains("()") {
            // `Type#field.` vs `path/CONST.`
            return if leaf.contains('#') {
                DescriptorKind::Field
            } else {
                DescriptorKind::Term
            };
        }
        // A method. `impl#[Self]` alone is inherent; a second bracket group
        // names the trait being implemented.
        if leaf.starts_with("impl#[") {
            return if leaf.matches("][").count() >= 1 {
                DescriptorKind::TraitImplMethod
            } else {
                DescriptorKind::Method
            };
        }
        // `Owner#method().` — the owner is a type, so this is the declaration
        // on that type. For a trait, that is exactly the dispatch site.
        return if leaf.contains('#') {
            DescriptorKind::TraitMethod
        } else {
            DescriptorKind::Function
        };
    }
    DescriptorKind::Unrecognized
}

/// How a reference to this callee was dispatched. See [`DispatchHint`].
pub fn dispatch_hint(callee_qualified: &str) -> DispatchHint {
    match descriptor_kind(callee_qualified) {
        DescriptorKind::TraitMethod => DispatchHint::ThroughTrait,
        _ => DispatchHint::Direct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PFX: &str = "rust-analyzer cargo some-crate 0.5.0 ";

    fn kind(desc: &str) -> DescriptorKind {
        descriptor_kind(&format!("{PFX}{desc}"))
    }

    #[test]
    fn every_shape_this_graph_actually_exhibits_is_classified() {
        assert_eq!(kind("types/ScoredChunk#"), DescriptorKind::Type);
        assert_eq!(kind("Verdict#"), DescriptorKind::Type);
        assert_eq!(kind("StartupOutcome#Failed#"), DescriptorKind::EnumVariant);
        assert_eq!(kind("DepEdge#from."), DescriptorKind::Field);
        assert_eq!(kind("workflow_cmd/HELP."), DescriptorKind::Term);
        assert_eq!(
            kind("runtime/streaming/run_synthesis_stream()."),
            DescriptorKind::Function
        );
        assert_eq!(
            kind("impl#[Runtime]handle_message()."),
            DescriptorKind::Method
        );
        assert_eq!(
            kind("peer_inference/impl#[MeshInferenceProvider][InferenceProvider]complete_stream_with_id()."),
            DescriptorKind::TraitImplMethod
        );
        assert_eq!(
            kind("traits/InferenceProvider#complete()."),
            DescriptorKind::TraitMethod
        );
        assert_eq!(kind("ids/define_id!"), DescriptorKind::Macro);
        assert_eq!(kind("crate/"), DescriptorKind::Module);
        assert_eq!(descriptor_kind("local 0"), DescriptorKind::Local);
    }

    #[test]
    fn the_case_the_stored_kind_column_gets_wrong() {
        // `sovereign_contracts::types::routing::Intent` is an ENUM and the
        // graph's `kind` column calls it `constructor` (measured 2026-08-19).
        // The descriptor was right the whole time.
        let intent = "rust-analyzer cargo sovereign-contracts 0.5.0 types/routing/Intent#";
        assert_eq!(descriptor_kind(intent), DescriptorKind::Type);
        assert!(descriptor_kind(intent).is_type_like());
        assert!(!descriptor_kind(intent).is_callable());
    }

    #[test]
    fn trait_dispatch_is_visible_where_ref_kind_says_direct_for_everything() {
        // The measured dyn site: `Arc<dyn InferenceProvider>` @ streaming.rs:208.
        // All 1,564,645 edges carry ref_kind='direct'; the descriptor does not.
        let via_trait =
            "rust-analyzer cargo sovereign-contracts 0.5.0 traits/InferenceProvider#complete_stream_with_id_and_finish().";
        let concrete =
            "rust-analyzer cargo sovereign-mesh 0.5.0 peer_inference/impl#[MeshInferenceProvider][InferenceProvider]complete_stream_with_id_and_finish().";
        assert_eq!(dispatch_hint(via_trait), DispatchHint::ThroughTrait);
        assert_eq!(dispatch_hint(concrete), DispatchHint::Direct);
        assert_eq!(
            dispatch_hint("rust-analyzer cargo c 0.1.0 m/plain_fn()."),
            DispatchHint::Direct
        );
    }

    #[test]
    fn the_shapes_a_live_graph_audit_found_missing() {
        // Validating the instrument before the result (ARCH §18.4): a first
        // pass left 25,374 of 313,741 rows (8.1%) unrecognized. Three real
        // shapes, all confirmed against the graph 2026-08-19.
        assert_eq!(
            kind("registry/impl#[Registry][Default]"),
            DescriptorKind::ImplBlock
        );
        assert_eq!(kind("adapter/pi/impl#[Adapter]"), DescriptorKind::ImplBlock);
        // scip-python emits these; the 5-token prefix parses the same way.
        assert_eq!(
            descriptor_kind("scip-python python . abc123 `gym.m`/boot_ci().(a)"),
            DescriptorKind::Parameter
        );
        assert_eq!(
            descriptor_kind("scip-python python . abc123 `gym.m`/__init__:"),
            DescriptorKind::Meta
        );
    }

    #[test]
    fn leaf_name_strips_the_terminator() {
        assert_eq!(
            leaf_name(&format!("{PFX}types/ScoredChunk#")),
            "ScoredChunk"
        );
        assert_eq!(leaf_name(&format!("{PFX}Verdict#")), "Verdict");
        assert_eq!(leaf_name(&format!("{PFX}m/run().")), "run()");
        assert_eq!(leaf_name(&format!("{PFX}ids/define_id!")), "define_id");
    }

    #[test]
    fn an_unknown_shape_is_unrecognized_rather_than_guessed() {
        assert_eq!(kind("no_terminator"), DescriptorKind::Unrecognized);
        assert_eq!(kind(""), DescriptorKind::Unrecognized);
    }

    #[test]
    fn a_bare_descriptor_with_no_package_prefix_still_classifies() {
        assert_eq!(descriptor_kind("types/ScoredChunk#"), DescriptorKind::Type);
        assert_eq!(descriptor_of("types/ScoredChunk#"), "types/ScoredChunk#");
        assert_eq!(
            descriptor_of("rust-analyzer cargo c 0.1.0 types/Foo#"),
            "types/Foo#"
        );
    }
}
