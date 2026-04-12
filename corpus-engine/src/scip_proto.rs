//! Minimal SCIP protobuf types.
//!
//! These types mirror the subset of the SCIP protocol we need for call
//! graph construction. The full spec lives at:
//! <https://github.com/sourcegraph/scip/blob/main/scip.proto>
//!
//! Rather than pulling in `prost-build` and a `.proto` file at compile
//! time (which requires `protoc` in the build environment), we define
//! the wire-compatible structures directly with `prost::Message` derives.
//! Only the fields used by the call graph builder are included — unknown
//! fields are silently skipped by prost's decoder.

#![allow(clippy::derive_partial_eq_without_eq)]

/// Top-level SCIP index. A `.scip` file decodes to exactly one `Index`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Index {
    /// Metadata about the tool that produced this index.
    #[prost(message, optional, tag = "1")]
    pub metadata: Option<Metadata>,
    /// One entry per source file that was analyzed.
    #[prost(message, repeated, tag = "2")]
    pub documents: Vec<Document>,
    /// External symbols referenced but not defined in this index.
    #[prost(message, repeated, tag = "3")]
    pub external_symbols: Vec<SymbolInformation>,
}

/// Metadata about the indexer that produced this SCIP file.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Metadata {
    /// Version of the SCIP protocol (currently 1).
    #[prost(enumeration = "ProtocolVersion", tag = "1")]
    pub version: i32,
    /// Info about the tool that created this index.
    #[prost(message, optional, tag = "2")]
    pub tool_info: Option<ToolInfo>,
    /// URI of the project root (e.g. "file:///home/user/project").
    #[prost(string, tag = "3")]
    pub project_root: String,
    /// How positions are encoded in Occurrence.range.
    #[prost(enumeration = "TextEncoding", tag = "4")]
    pub text_document_encoding: i32,
}

/// Info about the indexer tool.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ToolInfo {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub version: String,
    #[prost(string, repeated, tag = "3")]
    pub arguments: Vec<String>,
}

/// A single analyzed source file.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Document {
    /// Language identifier (e.g. "rust", "typescript").
    #[prost(string, tag = "4")]
    pub language: String,
    /// Relative path from the project root.
    #[prost(string, tag = "1")]
    pub relative_path: String,
    /// Occurrences of symbols in this file (references, definitions).
    #[prost(message, repeated, tag = "2")]
    pub occurrences: Vec<Occurrence>,
    /// Symbols defined in this file.
    #[prost(message, repeated, tag = "3")]
    pub symbols: Vec<SymbolInformation>,
}

/// A single occurrence of a symbol in a source file.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Occurrence {
    /// Source range: [startLine, startCol, endLine, endCol] or
    /// [startLine, startCol, endCol] when start and end are on the same line.
    #[prost(int32, repeated, packed = "false", tag = "1")]
    pub range: Vec<i32>,
    /// The SCIP symbol string this occurrence refers to.
    #[prost(string, tag = "2")]
    pub symbol: String,
    /// Bitmask of `SymbolRole` values.
    #[prost(int32, tag = "3")]
    pub symbol_roles: i32,
    /// Override documentation for this occurrence.
    #[prost(string, repeated, tag = "4")]
    pub override_documentation: Vec<String>,
    /// Syntax kind (identifier, keyword, etc.).
    #[prost(enumeration = "SyntaxKind", tag = "5")]
    pub syntax_kind: i32,
    /// Diagnostics at this occurrence.
    #[prost(message, repeated, tag = "6")]
    pub diagnostics: Vec<Diagnostic>,
    /// Enclosing range for this occurrence.
    #[prost(int32, repeated, tag = "7")]
    pub enclosing_range: Vec<i32>,
}

/// Information about a symbol (definition-side metadata).
#[derive(Clone, PartialEq, prost::Message)]
pub struct SymbolInformation {
    /// The SCIP symbol string.
    #[prost(string, tag = "1")]
    pub symbol: String,
    /// Human-readable documentation.
    #[prost(string, repeated, tag = "3")]
    pub documentation: Vec<String>,
    /// Relationships to other symbols (e.g. implements, inherits).
    #[prost(message, repeated, tag = "4")]
    pub relationships: Vec<Relationship>,
    /// Symbol kind (function, class, etc.).
    #[prost(enumeration = "symbol_information::Kind", tag = "5")]
    pub kind: i32,
    /// Display name (without qualification).
    #[prost(string, tag = "6")]
    pub display_name: String,
    /// Enclosing symbol.
    #[prost(string, tag = "7")]
    pub enclosing_symbol: String,
}

pub mod symbol_information {
    /// Symbol kinds as defined by SCIP.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
    #[repr(i32)]
    pub enum Kind {
        UnspecifiedKind = 0,
        AbstractMethod = 1,
        Accessor = 2,
        Array = 3,
        Assertion = 4,
        AssociatedType = 5,
        Attribute = 6,
        Axiom = 7,
        Boolean = 8,
        Class = 9,
        Constant = 10,
        Constructor = 11,
        Contract = 12,
        DataType = 13,
        Delegate = 14,
        Enum = 15,
        EnumMember = 16,
        Error = 17,
        Event = 18,
        Fact = 19,
        Field = 20,
        File = 21,
        Function = 22,
        Getter = 23,
        Grammar = 24,
        Instance = 25,
        Interface = 26,
        Key = 27,
        Lang = 28,
        Lemma = 29,
        Library = 30,
        Macro = 31,
        Method = 32,
        MethodAlias = 33,
        MethodReceiver = 34,
        MethodSpecification = 35,
        Message = 36,
        Modifier = 37,
        Module = 38,
        Namespace = 39,
        Null = 40,
        Number = 41,
        Object = 42,
        Operator = 43,
        Package = 44,
        PackageObject = 45,
        Parameter = 46,
        ParameterLabel = 47,
        Pattern = 48,
        Predicate = 49,
        Property = 50,
        Protocol = 51,
        ProtocolMethod = 52,
        PureVirtualMethod = 53,
        Quasiquoter = 54,
        SelfParameter = 55,
        Setter = 56,
        Signature = 57,
        SingletonClass = 58,
        SingletonMethod = 59,
        StaticDataMember = 60,
        StaticEvent = 61,
        StaticField = 62,
        StaticMethod = 63,
        StaticProperty = 64,
        StaticVariable = 65,
        String = 66,
        Struct = 67,
        Subscript = 68,
        Tactic = 69,
        Theorem = 70,
        ThisParameter = 71,
        Trait = 72,
        TraitMethod = 73,
        Type = 74,
        TypeAlias = 75,
        TypeClass = 76,
        TypeClassMethod = 77,
        TypeFamily = 78,
        TypeParameter = 79,
        Union = 80,
        Value = 81,
        Variable = 82,
    }
}

/// A relationship between two symbols.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Relationship {
    #[prost(string, tag = "1")]
    pub symbol: String,
    #[prost(bool, tag = "2")]
    pub is_reference: bool,
    #[prost(bool, tag = "3")]
    pub is_implementation: bool,
    #[prost(bool, tag = "4")]
    pub is_type_definition: bool,
    #[prost(bool, tag = "5")]
    pub is_definition: bool,
}

/// Diagnostic (unused in our call graph builder, but needed for decoding).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Diagnostic {
    #[prost(enumeration = "Severity", tag = "1")]
    pub severity: i32,
    #[prost(string, tag = "2")]
    pub code: String,
    #[prost(string, tag = "3")]
    pub message: String,
    #[prost(string, tag = "4")]
    pub source: String,
    #[prost(message, repeated, tag = "5")]
    pub tags: Vec<DiagnosticTag>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DiagnosticTag {
    #[prost(enumeration = "diagnostic_tag::UnusedOrDeprecated", tag = "1")]
    pub tag: i32,
}

pub mod diagnostic_tag {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
    #[repr(i32)]
    pub enum UnusedOrDeprecated {
        UnspecifiedDiagnosticTag = 0,
        Unnecessary = 1,
        Deprecated = 2,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum Severity {
    UnspecifiedSeverity = 0,
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

// ─── Enumerations ────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ProtocolVersion {
    UnspecifiedProtocolVersion = 0,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum TextEncoding {
    UnspecifiedTextEncoding = 0,
    Utf8 = 1,
    Utf16 = 2,
}

/// Symbol roles as a bitmask. We only care about Definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymbolRole;

impl SymbolRole {
    pub const DEFINITION: i32 = 0x1;
    pub const IMPORT: i32 = 0x2;
    pub const WRITE_ACCESS: i32 = 0x4;
    pub const READ_ACCESS: i32 = 0x8;
    pub const GENERATED: i32 = 0x10;
    pub const TEST: i32 = 0x20;
    pub const FORWARD_DEFINITION: i32 = 0x40;
}

/// Syntax kind for occurrences. We only need the discriminants for
/// decoding — duplicates in the upstream proto are collapsed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum SyntaxKind {
    UnspecifiedSyntaxKind = 0,
    Comment = 1,
    PunctuationDelimiter = 2,
    PunctuationBracket = 3,
    Keyword = 4,
    // IdentifierKeyword and IdentifierModule are aliases (4, 14) in the
    // upstream proto. prost requires unique discriminants so we drop the
    // aliases — they decode identically.
    IdentifierOperator = 5,
    Identifier = 6,
    IdentifierBuiltin = 7,
    IdentifierNull = 8,
    IdentifierConstant = 9,
    IdentifierMutableGlobal = 10,
    IdentifierParameter = 11,
    IdentifierLocal = 12,
    IdentifierShadowed = 13,
    IdentifierNamespace = 14,
    IdentifierFunction = 15,
    IdentifierFunctionDefinition = 16,
    IdentifierMacro = 17,
    IdentifierMacroDefinition = 18,
    IdentifierType = 19,
    IdentifierBuiltinType = 20,
    IdentifierAttribute = 21,
    RegexEscape = 22,
    RegexRepeated = 23,
    RegexWildcard = 24,
    RegexDelimiter = 25,
    RegexJoin = 26,
    StringLiteral = 27,
    StringLiteralEscape = 28,
    StringLiteralSpecial = 29,
    StringLiteralKey = 30,
    CharacterLiteral = 31,
    NumericLiteral = 32,
    BooleanLiteral = 33,
    Tag = 34,
    TagAttribute = 35,
}

/// Extract a human-readable symbol name from a SCIP symbol string.
///
/// SCIP symbol strings look like:
///   `rust-analyzer cargo my_crate 0.1.0 src/lib.rs/MyStruct#method().`
///
/// We extract the last meaningful segment as the display name.
pub fn extract_symbol_name(scip_symbol: &str) -> String {
    // Strip trailing punctuation (`.`, `#`, `/`, `()`)
    let trimmed = scip_symbol
        .trim_end_matches('.')
        .trim_end_matches("()")
        .trim_end_matches('#');

    // Take the last path segment
    if let Some(pos) = trimmed.rfind(|c: char| c == '/' || c == '#' || c == '.') {
        trimmed[pos + 1..].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Map a SCIP SymbolInformation::Kind to a human-readable kind string.
pub fn kind_to_str(kind: i32) -> &'static str {
    use symbol_information::Kind;
    match Kind::try_from(kind) {
        Ok(Kind::Function) | Ok(Kind::StaticMethod) => "function",
        Ok(Kind::Method) | Ok(Kind::AbstractMethod)
        | Ok(Kind::TraitMethod) | Ok(Kind::ProtocolMethod)
        | Ok(Kind::PureVirtualMethod) => "method",
        Ok(Kind::Class) | Ok(Kind::SingletonClass) => "class",
        Ok(Kind::Struct) => "struct",
        Ok(Kind::Enum) => "enum",
        Ok(Kind::Trait) | Ok(Kind::Interface) | Ok(Kind::Protocol) => "trait",
        Ok(Kind::Module) | Ok(Kind::Namespace) | Ok(Kind::Package) => "module",
        Ok(Kind::Constant) | Ok(Kind::StaticVariable) | Ok(Kind::StaticField) => "const",
        Ok(Kind::Type) | Ok(Kind::TypeAlias) => "type",
        Ok(Kind::Constructor) => "constructor",
        Ok(Kind::Field) | Ok(Kind::Property) | Ok(Kind::StaticProperty) => "field",
        Ok(Kind::Variable) => "variable",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_symbol_name_rust_function() {
        assert_eq!(
            extract_symbol_name("rust-analyzer cargo my_crate 0.1.0 src/lib.rs/my_fn()."),
            "my_fn"
        );
    }

    #[test]
    fn extract_symbol_name_method() {
        assert_eq!(
            extract_symbol_name("rust-analyzer cargo my_crate 0.1.0 src/lib.rs/MyStruct#method()."),
            "method"
        );
    }

    #[test]
    fn extract_symbol_name_struct() {
        assert_eq!(
            extract_symbol_name("rust-analyzer cargo my_crate 0.1.0 src/lib.rs/MyStruct#"),
            "MyStruct"
        );
    }

    #[test]
    fn extract_symbol_name_simple() {
        assert_eq!(extract_symbol_name("foo"), "foo");
    }

    #[test]
    fn kind_to_str_maps_function() {
        assert_eq!(kind_to_str(symbol_information::Kind::Function as i32), "function");
    }

    #[test]
    fn kind_to_str_maps_method() {
        assert_eq!(kind_to_str(symbol_information::Kind::Method as i32), "method");
    }

    #[test]
    fn kind_to_str_unknown_for_zero() {
        assert_eq!(kind_to_str(0), "unknown");
    }
}
