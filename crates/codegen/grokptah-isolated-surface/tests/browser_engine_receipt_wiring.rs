//! Receipt produce/consume wiring for `--features browser-engine`.
//!
//! Run via:
//! `cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml --features browser-engine --test browser_engine_receipt_wiring -- --test-threads=1`

#![cfg(feature = "browser-engine")]

use grokptah_isolated_surface::{
    native_browser_engine_capture_authorized, ContainedBrowserBackend, HarnessErrorCode,
    IsolatedSurfaceBackend,
};

#[test]
fn receipt_gated_substrate_label_and_no_simulator_fallback() {
    let backend = ContainedBrowserBackend::new();
    assert_eq!(backend.substrate_mode_label(), "receipt_gated");
    assert!(!backend.isolation_proof_available());
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[test]
fn cargo_test_never_authorizes_native_browser_engine_capture() {
    assert!(!native_browser_engine_capture_authorized());
}

#[test]
fn boot_stays_fail_closed_without_native_capture_authorization() {
    let mut backend = ContainedBrowserBackend::new();
    let err = backend
        .boot()
        .expect_err("native boot unwired and unauthorized");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(!backend.is_booted());
    assert!(err.message.contains("receipt-gated"));
}

#[test]
fn public_capture_entry_still_fails_closed_without_receipt() {
    let err = grokptah_isolated_surface::admit_browser_engine_capture(vec![0x01, 0x02, 0x03], 8, 8)
        .expect_err("no public receipt mint");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(err.message.contains("not wired"));
}
