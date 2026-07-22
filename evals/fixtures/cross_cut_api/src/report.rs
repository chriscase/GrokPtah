use crate::{config, service, LegacyWidget};

pub fn summary() -> String {
    let w = config::default_widget();
    service::describe(&w)
}

pub fn pair() -> (LegacyWidget, LegacyWidget) {
    (
        LegacyWidget::new(1, "a"),
        LegacyWidget::new(2, "b"),
    )
}
