use crate::OldThing;

/// User-facing product name — MUST stay exactly "OldThing" for telemetry.
pub const PRODUCT_LABEL: &str = "OldThing";

pub fn banner(t: &OldThing) -> String {
    format!("{PRODUCT_LABEL}#{}", t.id)
}
