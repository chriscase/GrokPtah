//! ABI-only snapshot proof. No WebKit object is loaded by these tests.

use grokptah_isolated_surface::isolated_surface_admission_available;

#[test]
fn abi_proof_never_enables_isolated_surface_admission() {
    assert!(!isolated_surface_admission_available());
}

#[cfg(target_os = "macos")]
#[test]
fn callback_has_a_native_block_shape_without_invoking_webkit() {
    let callback = grokptah_isolated_surface::snapshot_callback();
    assert!(!block2::RcBlock::as_ptr(&callback).is_null());
}
