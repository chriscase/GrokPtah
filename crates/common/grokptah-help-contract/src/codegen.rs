//! One model, two generated artifacts.
//!
//! The JSON Schema and the TypeScript declarations are emitted from the single
//! [`model`] below, so they cannot disagree with each other. They cannot
//! disagree with the Rust types either: `dto_tests::model_matches_rust_serde`
//! serializes a populated value of every modelled type and asserts its JSON
//! keys are exactly the modelled fields, so adding a Rust field without
//! modelling it fails the build rather than shipping a schema that quietly
//! omits it.
//!
//! Field names are the Rust serde names — `snake_case`, not `camelCase`. A
//! renaming layer is a place where two spellings of the same field can drift;
//! keeping one spelling end to end means the generated TypeScript is checkable
//! against the wire bytes by inspection.
//!
//! Emission is deterministic: declarations are emitted in model order, object
//! keys in a fixed order, and the writer ends every file with exactly one
//! newline. Running the generator twice produces identical bytes, which is what
//! `verify` mode in the `help-codegen` binary asserts.

use std::fmt::Write as _;

/// A type as the generated artifacts refer to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    Str,
    U64,
    Usize,
    Bool,
    /// Another declaration in this model.
    Named(&'static str),
    Array(Box<TypeRef>),
    Optional(Box<TypeRef>),
}

impl TypeRef {
    fn array(inner: TypeRef) -> Self {
        Self::Array(Box::new(inner))
    }

    fn optional(inner: TypeRef) -> Self {
        Self::Optional(Box::new(inner))
    }

    fn typescript(&self) -> String {
        match self {
            Self::Str => "string".to_string(),
            Self::U64 | Self::Usize => "number".to_string(),
            Self::Bool => "boolean".to_string(),
            Self::Named(name) => (*name).to_string(),
            Self::Array(inner) => format!("readonly {}[]", inner.typescript()),
            Self::Optional(inner) => format!("{} | null", inner.typescript()),
        }
    }

    fn json_schema(&self) -> serde_json::Value {
        match self {
            Self::Str => serde_json::json!({ "type": "string" }),
            Self::U64 => serde_json::json!({ "type": "integer", "minimum": 0 }),
            Self::Usize => serde_json::json!({ "type": "integer", "minimum": 0 }),
            Self::Bool => serde_json::json!({ "type": "boolean" }),
            Self::Named(name) => serde_json::json!({ "$ref": format!("#/$defs/{name}") }),
            Self::Array(inner) => {
                serde_json::json!({ "type": "array", "items": inner.json_schema() })
            }
            Self::Optional(inner) => {
                serde_json::json!({ "oneOf": [inner.json_schema(), { "type": "null" }] })
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: &'static str,
    pub ty: TypeRef,
    pub doc: &'static str,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Struct {
        name: &'static str,
        doc: &'static str,
        fields: Vec<Field>,
    },
    /// A closed set of string values.
    StringEnum {
        name: &'static str,
        doc: &'static str,
        variants: &'static [&'static str],
    },
}

impl Decl {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Struct { name, .. } | Self::StringEnum { name, .. } => name,
        }
    }
}

fn field(name: &'static str, ty: TypeRef, doc: &'static str) -> Field {
    Field { name, ty, doc }
}

/// The complete generated contract.
#[must_use]
pub fn model() -> Vec<Decl> {
    use TypeRef::{Bool, Named, Str, U64, Usize};
    vec![
        Decl::StringEnum {
            name: "HelpVisibility",
            doc: "Who a source may be shown to. Only `public` may enter a published bundle.",
            variants: &["public", "gated", "operator"],
        },
        Decl::StringEnum {
            name: "HelpChunkKind",
            doc: "The part of an article a chunk was cut from.",
            variants: &["title", "summary", "body"],
        },
        Decl::StringEnum {
            name: "HelpTopic",
            doc: "Top-level grouping used by the Help Center navigation.",
            variants: &["getting-started", "providers", "computer-use", "operations"],
        },
        Decl::StringEnum {
            name: "HelpRedactionKind",
            doc: "What the validator removed before anything reached a renderer.",
            variants: &["secret", "path", "control", "bidi", "markup"],
        },
        Decl::StringEnum {
            name: "HelpPublicErrorCode",
            doc: "The complete public error vocabulary. Three codes, no detail: every \
                  authorization outcome collapses to `not_available` so a refusal cannot be \
                  read as a statement about what exists.",
            variants: &["not_available", "busy", "timeout"],
        },
        Decl::StringEnum {
            name: "HelpProjectionStatus",
            doc: "Lifecycle of one ask, as a renderer sees it.",
            variants: &["queued", "running", "answered", "abstained", "unavailable"],
        },
        Decl::Struct {
            name: "HelpSourceAnchor",
            doc: "A citation target: an exact repository path plus an exact heading.",
            fields: vec![
                field("id", Str, "Stable citation id used in answers."),
                field(
                    "path",
                    Str,
                    "Repository-relative path. Must exist in the tree.",
                ),
                field("heading", Str, "Exact Markdown heading text within `path`."),
                field(
                    "visibility",
                    Named("HelpVisibility"),
                    "Who may be shown this source.",
                ),
                field(
                    "digest",
                    Str,
                    "`domain_digest(source, [id, path, heading, visibility])`.",
                ),
            ],
        },
        Decl::Struct {
            name: "HelpChunk",
            doc: "A retrievable unit, digest-bound to the exact bytes a span indexes.",
            fields: vec![
                field(
                    "id",
                    Str,
                    "`${article_id}#${kind}.${ordinal}` — stable and citable.",
                ),
                field("article_id", Str, "Article this chunk was cut from."),
                field("kind", Named("HelpChunkKind"), "Which part of the article."),
                field("ordinal", Usize, "Stable position within that part."),
                field("text", Str, "The exact bytes a citation span indexes into."),
                field("locale", Str, "Locale of `text`."),
                field(
                    "source_ids",
                    TypeRef::array(Str),
                    "Sources backing this chunk. Never empty.",
                ),
                field(
                    "visibility",
                    Named("HelpVisibility"),
                    "Who may be shown this chunk.",
                ),
                field(
                    "digest",
                    Str,
                    "Digest over the chunk's identity and its exact text.",
                ),
            ],
        },
        Decl::Struct {
            name: "HelpArticle",
            doc: "One Help article in the single canonical corpus.",
            fields: vec![
                field("id", Str, "Stable article id."),
                field("title", Str, "Display title."),
                field("topic", Named("HelpTopic"), "Navigation grouping."),
                field("summary", Str, "One-sentence summary."),
                field("body", Str, "Full prose body."),
                field(
                    "aliases",
                    TypeRef::array(Str),
                    "Natural-language phrasings a user might type.",
                ),
                field(
                    "keywords",
                    TypeRef::array(Str),
                    "Expert / identifier terminology.",
                ),
                field(
                    "source_ids",
                    TypeRef::array(Str),
                    "Sources this article cites.",
                ),
                field(
                    "visibility",
                    Named("HelpVisibility"),
                    "Who may be shown this article.",
                ),
                field(
                    "capability_ids",
                    TypeRef::array(Str),
                    "Capabilities a principal must hold.",
                ),
                field(
                    "digest",
                    Str,
                    "Digest over the article's fields and its sources' digests.",
                ),
            ],
        },
        Decl::Struct {
            name: "HelpCorpus",
            doc: "The frozen, digest-bound corpus. Exactly one exists in the tree, and Rust \
                  and TypeScript read the same bytes of it.",
            fields: vec![
                field(
                    "schema_version",
                    Str,
                    "Bumped when a record's shape changes.",
                ),
                field("content_version", Str, "Bumped when the content changes."),
                field(
                    "sources",
                    TypeRef::array(Named("HelpSourceAnchor")),
                    "Every distinct source, sorted by id.",
                ),
                field(
                    "articles",
                    TypeRef::array(Named("HelpArticle")),
                    "Every article, sorted by id.",
                ),
                field(
                    "chunks",
                    TypeRef::array(Named("HelpChunk")),
                    "Every chunk, sorted by id.",
                ),
                field(
                    "digest",
                    Str,
                    "Digest over the article and chunk digests, in order.",
                ),
                field(
                    "source_digest",
                    Str,
                    "Digest over the cited `path#heading` set.",
                ),
            ],
        },
        Decl::Struct {
            name: "HelpAsk",
            doc: "Ask Help a question. The complete set of content a renderer supplies: an \
                  opaque session handle, a question, and a locale. No route, principal, \
                  capability, chunk, or source may be named here.",
            fields: vec![
                field("session", Str, "Opaque handle the host issued."),
                field("question", Str, "The user's question."),
                field(
                    "locale",
                    TypeRef::optional(Str),
                    "Preferred locale, if any.",
                ),
            ],
        },
        Decl::Struct {
            name: "HelpFollow",
            doc: "Poll an in-flight ask by its opaque handle.",
            fields: vec![
                field("session", Str, "Opaque handle the host issued."),
                field("handle", Str, "Opaque ask handle the host issued."),
            ],
        },
        Decl::Struct {
            name: "HelpCancelRequest",
            doc: "Cancel an in-flight ask by its opaque handle.",
            fields: vec![
                field("session", Str, "Opaque handle the host issued."),
                field("handle", Str, "Opaque ask handle the host issued."),
            ],
        },
        Decl::Struct {
            name: "HelpCitationProjection",
            doc: "A citation as a renderer sees it: where to look and the exact redacted quote.",
            fields: vec![
                field("source_id", Str, "Stable citation id."),
                field("path", Str, "Repository-relative path."),
                field("heading", Str, "Heading within that path."),
                field("quote", Str, "The exact quoted bytes, already redacted."),
            ],
        },
        Decl::Struct {
            name: "HelpClaimProjection",
            doc: "One validator-derived claim and the citations supporting it.",
            fields: vec![
                field("ordinal", Usize, "Position in the answer."),
                field("text", Str, "Plain text. Never markup."),
                field(
                    "citations",
                    TypeRef::array(Named("HelpCitationProjection")),
                    "Supporting citations.",
                ),
            ],
        },
        Decl::Struct {
            name: "HelpProjection",
            doc: "The renderer-facing result. An opaque handle, plain text, and no authority: \
                  there is no grant, admission, route, principal, capability, or transport in \
                  this type, and none can be constructed from it.",
            fields: vec![
                field(
                    "handle",
                    Str,
                    "Opaque; meaningful only to the host that issued it.",
                ),
                field("status", Named("HelpProjectionStatus"), "Lifecycle state."),
                field(
                    "claims",
                    TypeRef::array(Named("HelpClaimProjection")),
                    "Validated claims.",
                ),
                field(
                    "error",
                    TypeRef::optional(Named("HelpPublicErrorCode")),
                    "Coarse code, when there are no claims.",
                ),
                field(
                    "message",
                    TypeRef::optional(Str),
                    "Fixed message for `error`.",
                ),
            ],
        },
        Decl::Struct {
            name: "HelpRedactionCount",
            doc: "How many of one redaction kind fired, without saying what was redacted.",
            fields: vec![
                field("kind", Named("HelpRedactionKind"), "Which kind."),
                field("count", Usize, "How many."),
            ],
        },
        Decl::Struct {
            name: "HelpReceiptProjection",
            doc: "The zero-content receipt view. Counts, digests, and timings — never the \
                  question, the answer, a chunk, or a provider reply.",
            fields: vec![
                field("receipt_id", Str, "Stable receipt id."),
                field("run_id", Str, "The durable Run this attempt belongs to."),
                field("outcome", Str, "How the attempt ended."),
                field(
                    "send_certainty",
                    Str,
                    "Whether a provider request is known to have been sent.",
                ),
                field("claim_count", Usize, "Number of validated claims."),
                field("span_count", Usize, "Number of citation spans."),
                field(
                    "redactions",
                    TypeRef::array(Named("HelpRedactionCount")),
                    "Redaction counts by kind.",
                ),
                field("corpus_digest", Str, "Corpus the attempt ran against."),
                field(
                    "manifest_revision",
                    U64,
                    "Manifest revision the attempt ran against.",
                ),
                field("started_at_ms", U64, "Start time."),
                field("finished_at_ms", U64, "Finish time."),
                field("digest", Str, "Digest binding this receipt to its request."),
            ],
        },
        Decl::Struct {
            name: "HelpBoundsProjection",
            doc: "The executor's fixed bounds, exposed so a surface can render honest limits \
                  instead of guessing them.",
            fields: vec![
                field("max_concurrency", Usize, "Simultaneous provider attempts."),
                field(
                    "max_queued",
                    Usize,
                    "Waiting asks before new ones are refused.",
                ),
                field("deadline_ms", U64, "Wall-clock budget for one ask."),
                field(
                    "single_request",
                    Bool,
                    "Always true: exactly one provider request per ask.",
                ),
                field("tools_enabled", Bool, "Always false."),
                field("history_enabled", Bool, "Always false."),
                field("workspace_enabled", Bool, "Always false."),
                field("fallback_enabled", Bool, "Always false."),
            ],
        },
    ]
}

/// Escape a string for a TypeScript/JSON double-quoted literal.
fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// Wrap `doc` as a block comment at `indent`, at a fixed width.
fn doc_block(doc: &str, indent: &str) -> String {
    if doc.is_empty() {
        return String::new();
    }
    let mut words = doc.split_whitespace().peekable();
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    while let Some(word) = words.next() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() + indent.len() + 3 <= 88 {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
        if words.peek().is_none() && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
    }
    let mut out = format!("{indent}/**\n");
    for line in lines {
        let _ = writeln!(out, "{indent} * {line}");
    }
    let _ = writeln!(out, "{indent} */");
    out
}

/// The banner every generated file carries.
const BANNER: &str = "\
// @generated by `cargo run -p grokptah-help-contract --bin help-codegen`.
//
// Do not edit. This file is emitted from the Rust model in
// `crates/common/grokptah-help-contract/src/codegen.rs`, which is the single
// definition of the Semantic Help contract. `help-codegen --verify` fails the
// build when the committed bytes differ from a fresh emission, so a hand edit
// here is reverted by the next gate rather than surviving as a second source
// of truth.
";

/// Emit the TypeScript declarations.
#[must_use]
pub fn emit_typescript(decls: &[Decl]) -> String {
    let mut out = String::new();
    out.push_str(BANNER);
    out.push('\n');
    for decl in decls {
        match decl {
            Decl::StringEnum {
                name,
                doc,
                variants,
            } => {
                out.push_str(&doc_block(doc, ""));
                let body = variants
                    .iter()
                    .map(|variant| quote(variant))
                    .collect::<Vec<_>>()
                    .join(" | ");
                let _ = writeln!(out, "export type {name} = {body};\n");
            }
            Decl::Struct { name, doc, fields } => {
                out.push_str(&doc_block(doc, ""));
                let _ = writeln!(out, "export type {name} = {{");
                for f in fields {
                    out.push_str(&doc_block(f.doc, "  "));
                    let _ = writeln!(out, "  readonly {}: {};", f.name, f.ty.typescript());
                }
                out.push_str("};\n\n");
            }
        }
    }
    // Exactly one trailing newline.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// Emit the JSON Schema document.
#[must_use]
pub fn emit_json_schema(decls: &[Decl]) -> serde_json::Value {
    let mut defs = serde_json::Map::new();
    for decl in decls {
        let value = match decl {
            Decl::StringEnum { doc, variants, .. } => serde_json::json!({
                "description": doc,
                "type": "string",
                "enum": variants,
            }),
            Decl::Struct { doc, fields, .. } => {
                let mut properties = serde_json::Map::new();
                let mut required: Vec<String> = Vec::new();
                for f in fields {
                    let mut schema = f.ty.json_schema();
                    if let Some(object) = schema.as_object_mut() {
                        object.insert(
                            "description".to_string(),
                            serde_json::Value::String(f.doc.to_string()),
                        );
                    }
                    properties.insert(f.name.to_string(), schema);
                    required.push(f.name.to_string());
                }
                serde_json::json!({
                    "description": doc,
                    "type": "object",
                    "additionalProperties": false,
                    "properties": properties,
                    "required": required,
                })
            }
        };
        defs.insert(decl.name().to_string(), value);
    }
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://grokptah.dev/schemas/grokptah-help.v1.schema.json",
        "title": "GrokPtah Semantic Help contract",
        "description":
            "Generated from the Rust model in grokptah-help-contract. The host owns every \
             type here; a renderer may send only HelpAsk, HelpFollow, and HelpCancelRequest.",
        "$defs": defs,
    })
}

/// Render the schema as the exact bytes written to disk: two-space indented,
/// key order preserved from the model, one trailing newline.
#[must_use]
pub fn render_json_schema(value: &serde_json::Value) -> String {
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    serde::Serialize::serialize(value, &mut serializer).expect("schema serializes");
    let mut text = String::from_utf8(buffer).expect("schema is utf-8");
    text.push('\n');
    text
}
