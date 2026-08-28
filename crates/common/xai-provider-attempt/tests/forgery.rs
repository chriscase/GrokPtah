#[test]
fn cross_crate_lattice_forgery_is_rejected_at_compile_time() {
    trybuild::TestCases::new().compile_fail("tests/ui/forged_authority.rs");
}
