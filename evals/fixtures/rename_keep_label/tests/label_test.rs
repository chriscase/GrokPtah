use label_api::{display, types::NewThing};

#[test]
fn type_renamed_and_works() {
    let t = NewThing::new(7);
    assert_eq!(t.id, 7);
    assert_eq!(display::banner(&t), "OldThing#7");
}

#[test]
fn product_label_string_unchanged() {
    assert_eq!(display::PRODUCT_LABEL, "OldThing");
}
