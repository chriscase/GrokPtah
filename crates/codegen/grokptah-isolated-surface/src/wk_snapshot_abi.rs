//! Compile-time proof of the narrow WebKit snapshot callback ABI.
//!
//! This module deliberately does not load WebKit, create a `WKWebView`, invoke
//! the selector, or inspect pixels. It proves only that the ordinary macOS
//! crate can construct a native Objective-C block and encode the dynamic
//! selector call without linking the generated WebKit bindings. Capture,
//! provenance, pixel layout, and admission remain separate fail-closed work.

use block2::{Block, RcBlock};
use objc2::runtime::AnyObject;

/// Callback shape emitted by `WKWebView` for `takeSnapshotWithConfiguration:`.
pub type SnapshotCallback = dyn Fn(*mut AnyObject, *mut AnyObject);

/// Construct a native block with the exact two-object completion signature.
pub fn snapshot_callback() -> RcBlock<SnapshotCallback> {
    RcBlock::new(|_image: *mut AnyObject, _error: *mut AnyObject| {})
}

/// Encode the dynamic WebKit selector without loading or invoking WebKit.
///
/// The caller must provide a real WebKit view/configuration only in a future,
/// separately authorized native capture path. This proof function performs no
/// Objective-C message at runtime in tests; it exists to keep the ABI seam
/// type-checked while admission remains disabled.
///
/// # Safety
///
/// The caller must ensure that `view` is a live `WKWebView`, `configuration`
/// is either null or a live `WKSnapshotConfiguration`, and `callback` remains
/// valid for the asynchronous Objective-C call. This function is not used by
/// ordinary tests or any admission path.
pub unsafe fn send_snapshot_with_callback(
    view: &AnyObject,
    configuration: Option<&AnyObject>,
    callback: &Block<SnapshotCallback>,
) {
    let _: () = unsafe {
        objc2::msg_send![
            view,
            takeSnapshotWithConfiguration: configuration,
            completionHandler: callback
        ]
    };
}
