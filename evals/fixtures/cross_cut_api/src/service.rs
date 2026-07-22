use crate::LegacyWidget;

pub fn describe(w: &LegacyWidget) -> String {
    format!("widget#{}:{}", w.id, w.label)
}

pub fn relabel(w: LegacyWidget, label: &str) -> LegacyWidget {
    LegacyWidget::new(w.id, label)
}
