//! Live Mac host sentinel collection for Sep 18 physical proof (#288).
//!
//! Reads pointer, foreground, clipboard, unrelated-window, and main-checkout
//! fence evidence via native APIs. Fails closed when Accessibility trust or
//! other required host reads are unavailable — never emits synthetic PASS values.

use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use sha2::{Digest, Sha256};

    use crate::error::{HarnessError, HarnessResult};
    use crate::main_checkout_fence::collect_main_checkout_fence;
    use crate::sentinel::HostSentinelSnapshot;

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(source: *const c_void) -> *const c_void;
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
        fn AXUIElementCopyAttributeValue(
            element: *const c_void,
            attribute: *const c_void,
            value: *mut *const c_void,
        ) -> i32;
        fn AXUIElementCopyAttributeValues(
            element: *const c_void,
            attribute: *const c_void,
            index: i32,
            max_values: i32,
            values: *mut *const c_void,
        ) -> i32;
    }

    const K_AX_ERROR_SUCCESS: i32 = 0;
    const K_AX_FOCUSED_WINDOW_ATTRIBUTE: &str = "AXFocusedWindow";
    const K_AX_WINDOWS_ATTRIBUTE: &str = "AXWindows";
    const K_AX_TITLE_ATTRIBUTE: &str = "AXTitle";
    const K_AX_ROLE_ATTRIBUTE: &str = "AXRole";

    pub fn collect(checkout_path: &str) -> HarnessResult<HostSentinelSnapshot> {
        if !ax_is_trusted() {
            return Err(HarnessError::backend_unavailable(
                "Mac host sentinel collection requires Accessibility trust for foreground/unrelated window evidence",
            ));
        }

        let main_checkout_fence = collect_main_checkout_fence(checkout_path)?;
        let (pointer_x, pointer_y) = collect_pointer()?;
        let (foreground_app_id, foreground_window_id) = collect_foreground_window()?;
        let clipboard_digest = collect_clipboard_digest()?;
        let (unrelated_window_app_id, unrelated_window_id, unrelated_window_title_hash) =
            collect_unrelated_window(&foreground_app_id, &foreground_window_id)?;

        Ok(HostSentinelSnapshot {
            pointer_x,
            pointer_y,
            foreground_app_id,
            foreground_window_id,
            clipboard_digest,
            unrelated_window_app_id,
            unrelated_window_id,
            unrelated_window_title_hash,
            main_checkout_fence,
        })
    }

    fn ax_is_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    fn collect_pointer() -> HarnessResult<(i32, i32)> {
        unsafe {
            let event = CGEventCreate(std::ptr::null());
            if event.is_null() {
                return Err(HarnessError::backend_unavailable(
                    "Mac host sentinel pointer read unavailable (CGEventCreate returned null)",
                ));
            }
            let location = CGEventGetLocation(event);
            CFRelease(event);
            Ok((location.x.round() as i32, location.y.round() as i32))
        }
    }

    fn collect_foreground_window() -> HarnessResult<(String, String)> {
        let workspace = ns_workspace()?;
        let frontmost: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![&*workspace, frontmostApplication] };
        let frontmost = frontmost.ok_or_else(|| {
            HarnessError::backend_unavailable("Mac host sentinel foreground app unavailable")
        })?;
        let bundle_id: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![&*frontmost, bundleIdentifier] };
        let bundle_id = nsstring_to_string(bundle_id.as_deref()).ok_or_else(|| {
            HarnessError::backend_unavailable(
                "Mac host sentinel foreground bundleIdentifier unavailable",
            )
        })?;
        let pid: i32 = unsafe { objc2::msg_send![&*frontmost, processIdentifier] };
        let window_id = ax_focused_window_id(pid).ok_or_else(|| {
            HarnessError::backend_unavailable(
                "Mac host sentinel foreground window unavailable via AX",
            )
        })?;
        Ok((bundle_id, window_id))
    }

    fn collect_clipboard_digest() -> HarnessResult<String> {
        let pasteboard = general_pasteboard().ok_or_else(|| {
            HarnessError::backend_unavailable("Mac host sentinel clipboard unavailable")
        })?;
        let change_count: isize = unsafe { objc2::msg_send![&*pasteboard, changeCount] };
        let types: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*pasteboard, types] };
        let mut hasher = Sha256::new();
        hasher.update(change_count.to_le_bytes());
        if let Some(types) = types {
            let count: usize = unsafe { objc2::msg_send![&*types, count] };
            hasher.update((count as u64).to_le_bytes());
            for index in 0..count {
                let item: Retained<AnyObject> =
                    unsafe { objc2::msg_send![&*types, objectAtIndex: index] };
                if let Some(type_name) = nsstring_to_string(Some(&*item)) {
                    hasher.update(type_name.as_bytes());
                    hasher.update(b"\0");
                }
            }
        }
        Ok(format!("sha256:{:x}", hasher.finalize()))
    }

    fn collect_unrelated_window(
        foreground_app_id: &str,
        foreground_window_id: &str,
    ) -> HarnessResult<(String, String, String)> {
        let workspace = ns_workspace()?;
        let apps: Retained<AnyObject> =
            unsafe { objc2::msg_send![&*workspace, runningApplications] };
        let count: usize = unsafe { objc2::msg_send![&*apps, count] };
        for index in 0..count {
            let app: Retained<AnyObject> =
                unsafe { objc2::msg_send![&*apps, objectAtIndex: index] };
            let bundle_id: Option<Retained<AnyObject>> =
                unsafe { objc2::msg_send![&*app, bundleIdentifier] };
            let Some(bundle_id) = nsstring_to_string(bundle_id.as_deref()) else {
                continue;
            };
            if bundle_id == foreground_app_id {
                continue;
            }
            let pid: i32 = unsafe { objc2::msg_send![&*app, processIdentifier] };
            if let Some((window_id, title_hash)) =
                ax_first_window_fingerprint(pid, foreground_window_id)
            {
                return Ok((bundle_id, window_id, title_hash));
            }
        }
        Err(HarnessError::backend_unavailable(
            "Mac host sentinel unrelated window unavailable via AX",
        ))
    }

    fn ax_focused_window_id(pid: i32) -> Option<String> {
        unsafe {
            let app = AXUIElementCreateApplication(pid);
            if app.is_null() {
                return None;
            }
            let attr = cfstring(K_AX_FOCUSED_WINDOW_ATTRIBUTE);
            let mut window: *const c_void = std::ptr::null();
            let status =
                AXUIElementCopyAttributeValue(app, attr, &mut window as *mut *const c_void);
            CFRelease(app);
            CFRelease(attr);
            if status != K_AX_ERROR_SUCCESS || window.is_null() {
                return None;
            }
            let fingerprint = ax_window_fingerprint(window);
            CFRelease(window);
            fingerprint.map(|(window_id, _)| window_id)
        }
    }

    fn ax_first_window_fingerprint(pid: i32, exclude_window_id: &str) -> Option<(String, String)> {
        unsafe {
            let app = AXUIElementCreateApplication(pid);
            if app.is_null() {
                return None;
            }
            let attr = cfstring(K_AX_WINDOWS_ATTRIBUTE);
            let mut windows: *const c_void = std::ptr::null();
            let status = AXUIElementCopyAttributeValues(app, attr, 0, 100, &mut windows);
            CFRelease(app);
            CFRelease(attr);
            if status != K_AX_ERROR_SUCCESS || windows.is_null() {
                return None;
            }
            // Copy-rule array; CFArrayGetValueAtIndex is Get-rule — do not CFRelease elements.
            let count = cf_array_len(windows);
            for index in 0..count {
                let window = cf_array_value_at(windows, index);
                if window.is_null() {
                    continue;
                }
                if let Some((window_id, title_hash)) = ax_window_fingerprint(window) {
                    if window_id != exclude_window_id {
                        CFRelease(windows);
                        return Some((window_id, title_hash));
                    }
                }
            }
            CFRelease(windows);
            None
        }
    }

    fn ax_window_fingerprint(window: *const c_void) -> Option<(String, String)> {
        unsafe {
            let role_attr = cfstring(K_AX_ROLE_ATTRIBUTE);
            let mut role_value: *const c_void = std::ptr::null();
            let role_status = AXUIElementCopyAttributeValue(
                window,
                role_attr,
                &mut role_value as *mut *const c_void,
            );
            CFRelease(role_attr);
            let role = if role_status == K_AX_ERROR_SUCCESS && !role_value.is_null() {
                let role = cfstring_to_string(role_value);
                CFRelease(role_value);
                role
            } else {
                None
            };

            let title_attr = cfstring(K_AX_TITLE_ATTRIBUTE);
            let mut title_value: *const c_void = std::ptr::null();
            let title_status = AXUIElementCopyAttributeValue(
                window,
                title_attr,
                &mut title_value as *mut *const c_void,
            );
            CFRelease(title_attr);
            let title = if title_status == K_AX_ERROR_SUCCESS && !title_value.is_null() {
                let title = cfstring_to_string(title_value).unwrap_or_default();
                CFRelease(title_value);
                title
            } else {
                String::new()
            };

            let mut hasher = Sha256::new();
            hasher.update(role.unwrap_or_default().as_bytes());
            hasher.update(b"|");
            hasher.update(title.as_bytes());
            let title_hash = format!("sha256:{:x}", hasher.finalize());
            let window_id = format!("ax-window:{title_hash}");
            Some((window_id, title_hash))
        }
    }

    fn ns_workspace() -> HarnessResult<Retained<AnyObject>> {
        if !appkit_loaded() {
            return Err(HarnessError::backend_unavailable(
                "Mac host sentinel AppKit unavailable",
            ));
        }
        let cls = AnyClass::get(c"NSWorkspace").ok_or_else(|| {
            HarnessError::backend_unavailable("Mac host sentinel NSWorkspace unavailable")
        })?;
        let shared: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, sharedWorkspace] };
        shared.ok_or_else(|| {
            HarnessError::backend_unavailable(
                "Mac host sentinel NSWorkspace.sharedWorkspace unavailable",
            )
        })
    }

    fn general_pasteboard() -> Option<Retained<AnyObject>> {
        if !appkit_loaded() {
            return None;
        }
        let cls = AnyClass::get(c"NSPasteboard")?;
        unsafe { objc2::msg_send![cls, generalPasteboard] }
    }

    fn appkit_loaded() -> bool {
        static LOADED: OnceLock<bool> = OnceLock::new();
        *LOADED.get_or_init(|| {
            let path = c"/System/Library/Frameworks/AppKit.framework/AppKit";
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
            !handle.is_null()
        })
    }

    fn nsstring_to_string(object: Option<&AnyObject>) -> Option<String> {
        let object = object?;
        let utf8: *const i8 = unsafe { objc2::msg_send![object, UTF8String] };
        if utf8.is_null() {
            return None;
        }
        unsafe {
            std::ffi::CStr::from_ptr(utf8)
                .to_str()
                .ok()
                .map(str::to_owned)
        }
    }

    fn cfstring(value: &str) -> *const c_void {
        unsafe {
            #[link(name = "CoreFoundation", kind = "framework")]
            extern "C" {
                fn CFStringCreateWithCString(
                    alloc: *const c_void,
                    c_str: *const i8,
                    encoding: u32,
                ) -> *const c_void;
            }
            const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
            CFStringCreateWithCString(
                std::ptr::null(),
                std::ffi::CString::new(value).expect("cfstring").as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            )
        }
    }

    fn cfstring_to_string(value: *const c_void) -> Option<String> {
        unsafe {
            #[link(name = "CoreFoundation", kind = "framework")]
            extern "C" {
                fn CFStringGetCStringPtr(cstr: *const c_void, encoding: u32) -> *const i8;
                fn CFStringGetLength(cstr: *const c_void) -> i64;
                fn CFStringGetCString(
                    cstr: *const c_void,
                    buffer: *mut i8,
                    buffer_size: i64,
                    encoding: u32,
                ) -> bool;
            }
            const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
            let direct = CFStringGetCStringPtr(value, K_CF_STRING_ENCODING_UTF8);
            if !direct.is_null() {
                return std::ffi::CStr::from_ptr(direct)
                    .to_str()
                    .ok()
                    .map(str::to_owned);
            }
            let len = CFStringGetLength(value);
            let mut buffer = vec![0i8; (len * 4 + 1) as usize];
            if CFStringGetCString(
                value,
                buffer.as_mut_ptr(),
                buffer.len() as i64,
                K_CF_STRING_ENCODING_UTF8,
            ) {
                std::ffi::CStr::from_ptr(buffer.as_ptr())
                    .to_str()
                    .ok()
                    .map(str::to_owned)
            } else {
                None
            }
        }
    }

    fn cf_array_len(array: *const c_void) -> isize {
        unsafe {
            #[link(name = "CoreFoundation", kind = "framework")]
            extern "C" {
                fn CFArrayGetCount(array: *const c_void) -> i64;
            }
            CFArrayGetCount(array) as isize
        }
    }

    fn cf_array_value_at(array: *const c_void, index: isize) -> *const c_void {
        unsafe {
            #[link(name = "CoreFoundation", kind = "framework")]
            extern "C" {
                fn CFArrayGetValueAtIndex(array: *const c_void, index: i64) -> *const c_void;
            }
            CFArrayGetValueAtIndex(array, index as i64)
        }
    }
}

/// Whether the native Mac collector can exist on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacHostSentinelPlatformSupport {
    NativeCollectorAvailable,
    UnavailableNonMacOs,
}

/// Cross-platform platform support probe for tests and honest nonclaims.
pub fn mac_host_sentinel_platform_support() -> MacHostSentinelPlatformSupport {
    #[cfg(target_os = "macos")]
    {
        MacHostSentinelPlatformSupport::NativeCollectorAvailable
    }
    #[cfg(not(target_os = "macos"))]
    {
        MacHostSentinelPlatformSupport::UnavailableNonMacOs
    }
}

#[cfg(target_os = "macos")]
pub use platform::collect;
