use crate::LegacyWidget;

pub fn ids(ws: &[LegacyWidget]) -> Vec<u64> {
    ws.iter().map(|w| w.id).collect()
}
