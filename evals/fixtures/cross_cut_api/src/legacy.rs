//! Compatibility re-exports — still uses the old name.
pub use crate::LegacyWidget as WidgetCompat;

pub fn from_compat(w: WidgetCompat) -> crate::LegacyWidget {
    w
}
