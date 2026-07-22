use perm_rules::{apply_always_allow, is_auto_approved, PermissionState};

#[test]
fn always_allow_is_per_tool_not_global() {
    let mut s = PermissionState::default();
    apply_always_allow(&mut s, "shell");
    assert!(is_auto_approved(&s, "shell"));
    assert!(
        !is_auto_approved(&s, "write_file"),
        "AlwaysAllow on shell must not approve write_file"
    );
    assert!(
        !s.always_approve,
        "always_approve is Settings YOLO only; AlwaysAllow must not set it"
    );
}

#[test]
fn multiple_tools() {
    let mut s = PermissionState::default();
    apply_always_allow(&mut s, "read_file");
    apply_always_allow(&mut s, "grep");
    assert!(is_auto_approved(&s, "read_file"));
    assert!(is_auto_approved(&s, "grep"));
    assert!(!is_auto_approved(&s, "shell"));
    assert!(!s.always_approve);
}
