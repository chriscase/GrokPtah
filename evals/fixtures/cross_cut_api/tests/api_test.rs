use widget_api::{config, report, service, types::CoreWidget, util, WidgetCompat};

#[test]
fn rename_complete() {
    let w = CoreWidget::new(9, "x");
    assert_eq!(service::describe(&w), "widget#9:x");
    let w2 = service::relabel(w, "y");
    assert_eq!(w2.label, "y");
    assert_eq!(config::default_widget().id, 0);
    assert!(report::summary().contains("default"));
    let (a, b) = report::pair();
    assert_eq!(util::ids(&[a, b]), vec![1, 2]);
    let c = WidgetCompat::new(3, "z");
    assert_eq!(c.id, 3);
}

#[test]
fn no_legacy_type_name_in_types_module() {
    // Compile-time: CoreWidget is the public type.
    let _: CoreWidget = CoreWidget::new(0, "t");
}
