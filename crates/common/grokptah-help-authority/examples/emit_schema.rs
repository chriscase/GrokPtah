//! Emits the Help authority JSON Schema.
//!
//! Regenerate the checked-in document with:
//!   cargo run -p grokptah-help-authority --example emit_schema \
//!     > crates/common/grokptah-help-authority/schema/help-authority.v1.schema.json
fn main() {
    print!("{}", grokptah_help_authority::schema::json_schema_string());
}
