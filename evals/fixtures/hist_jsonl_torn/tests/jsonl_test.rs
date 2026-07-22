use session_log::load_jsonl;

#[test]
fn complete_lines_load() {
    let text = r#"{"id":1}
{"id":2}
"#;
    let v = load_jsonl(text).unwrap();
    assert_eq!(v.len(), 2);
}

#[test]
fn torn_trailing_line_is_skipped_not_fatal() {
    // Simulates crash mid-write of the last record.
    let text = "{\"id\":1}\n{\"id\":2}\n{\"id\":";
    let v = load_jsonl(text).expect("torn trailing line must not fail the whole load");
    assert_eq!(v.len(), 2);
    assert_eq!(v[0]["id"], 1);
    assert_eq!(v[1]["id"], 2);
}
