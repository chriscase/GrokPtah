//! Smallest honest native WKWebView raster for receipt-gated capture.
//!
//! Creates a real offscreen `WKWebView`, loads a tiny HTML fixture, and copies
//! RGBA8 from `NSBitmapImageRep` via `cacheDisplayInRect:toBitmapImageRep:`.
//! This is not `takeSnapshotWithConfiguration:` ABI proof (#564), not isolation
//! PASS, and never enables admission. Fail-closed when WebKit or AppKit is
//! unavailable.

use std::ffi::CString;
use std::time::{Duration, Instant};

use objc2::encode::{Encode, Encoding};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::sel;

use crate::error::{HarnessError, HarnessResult};

pub(crate) struct NativeWkRaster {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const FIXTURE_HTML: &str =
    "<!doctype html><html><body style=\"margin:0;background:#c41e3a\"></body></html>";
const LOGICAL_WIDTH: f64 = 8.0;
const LOGICAL_HEIGHT: f64 = 8.0;
const MAX_PIXEL_EDGE: u32 = 64;
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn rasterize_fixture() -> HarnessResult<NativeWkRaster> {
    if !is_main_thread() {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot requires the process main thread",
        ));
    }
    if !webkit_loaded() {
        return Err(HarnessError::backend_unavailable(
            "WebKit.framework is unavailable for live WK snapshot",
        ));
    }
    let Some(_) = AnyClass::get(c"WKWebView") else {
        return Err(HarnessError::backend_unavailable(
            "WKWebView class unavailable",
        ));
    };
    let Some(_) = AnyClass::get(c"WKWebViewConfiguration") else {
        return Err(HarnessError::backend_unavailable(
            "WKWebViewConfiguration class unavailable",
        ));
    };
    ensure_ns_application()?;

    let html = nsstring(FIXTURE_HTML).ok_or_else(|| {
        HarnessError::backend_unavailable("live WK snapshot HTML NSString unavailable")
    })?;
    let config_cls = AnyClass::get(c"WKWebViewConfiguration")
        .ok_or_else(|| HarnessError::backend_unavailable("WKWebViewConfiguration unavailable"))?;
    let config: Retained<AnyObject> = unsafe { objc2::msg_send![config_cls, new] };

    let webview_cls = AnyClass::get(c"WKWebView")
        .ok_or_else(|| HarnessError::backend_unavailable("WKWebView unavailable"))?;
    let frame = cg_rect(0.0, 0.0, LOGICAL_WIDTH, LOGICAL_HEIGHT);
    let webview_alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![webview_cls, alloc] };
    let webview: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![webview_alloc, initWithFrame: frame, configuration: &*config] };
    let webview = webview
        .ok_or_else(|| HarnessError::backend_unavailable("WKWebView initWithFrame unavailable"))?;

    let _window = attach_offscreen_window(&webview, frame)?;

    let _navigation: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![&*webview, loadHTMLString: &*html, baseURL: None::<&AnyObject>] };
    wait_until_not_loading(&webview)?;

    let _: () = unsafe { objc2::msg_send![&*webview, layoutSubtreeIfNeeded] };
    pump_runloop_briefly();

    copy_rgba8_from_view(&webview, frame)
}

fn ensure_ns_application() -> HarnessResult<()> {
    let cls = AnyClass::get(c"NSApplication").ok_or_else(|| {
        HarnessError::backend_unavailable("NSApplication unavailable for live WK snapshot")
    })?;
    let app: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, sharedApplication] };
    let app = app.ok_or_else(|| {
        HarnessError::backend_unavailable("NSApplication.sharedApplication unavailable")
    })?;
    // Accessory: do not become a regular foreground app (avoids dock/TCC prompts).
    let _: bool = unsafe { objc2::msg_send![&*app, setActivationPolicy: 1isize] };
    Ok(())
}

fn attach_offscreen_window(
    webview: &AnyObject,
    frame: CGRect,
) -> HarnessResult<Option<Retained<AnyObject>>> {
    let Some(window_cls) = AnyClass::get(c"NSWindow") else {
        return Ok(None);
    };
    let alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![window_cls, alloc] };
    // NSWindowStyleMaskBorderless = 0, NSBackingStoreBuffered = 2.
    let window: Option<Retained<AnyObject>> = unsafe {
        objc2::msg_send![
            alloc,
            initWithContentRect: frame,
            styleMask: 0usize,
            backing: 2usize,
            defer: false
        ]
    };
    let Some(window) = window else {
        return Ok(None);
    };
    let _: () = unsafe { objc2::msg_send![&*window, setReleasedWhenClosed: false] };
    let _: () = unsafe { objc2::msg_send![&*window, setContentView: webview] };
    let _: () = unsafe { objc2::msg_send![&*window, orderBack: None::<&AnyObject>] };
    Ok(Some(window))
}

fn wait_until_not_loading(webview: &AnyObject) -> HarnessResult<()> {
    let started = Instant::now();
    loop {
        if started.elapsed() > LOAD_TIMEOUT {
            return Err(HarnessError::backend_unavailable(
                "live WK snapshot timed out waiting for WKWebView load",
            ));
        }
        let loading: bool = unsafe { objc2::msg_send![webview, isLoading] };
        if !loading {
            pump_runloop_briefly();
            return Ok(());
        }
        pump_runloop_briefly();
    }
}

fn copy_rgba8_from_view(webview: &AnyObject, frame: CGRect) -> HarnessResult<NativeWkRaster> {
    let scale = backing_scale(webview);
    let width = clamp_edge((LOGICAL_WIDTH * scale).round() as u32);
    let height = clamp_edge((LOGICAL_HEIGHT * scale).round() as u32);
    let color_space = nsstring("NSCalibratedRGBColorSpace").ok_or_else(|| {
        HarnessError::backend_unavailable("NSCalibratedRGBColorSpace NSString unavailable")
    })?;
    let rep_cls = AnyClass::get(c"NSBitmapImageRep")
        .ok_or_else(|| HarnessError::backend_unavailable("NSBitmapImageRep unavailable"))?;
    let alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![rep_cls, alloc] };
    let bytes_per_row = (width as isize).saturating_mul(4);
    let rep: Option<Retained<AnyObject>> = unsafe {
        objc2::msg_send![
            alloc,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
            pixelsWide: width as isize,
            pixelsHigh: height as isize,
            bitsPerSample: 8isize,
            samplesPerPixel: 4isize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: &*color_space,
            bytesPerRow: bytes_per_row,
            bitsPerPixel: 32isize
        ]
    };
    let rep = rep.ok_or_else(|| {
        HarnessError::backend_unavailable("NSBitmapImageRep init for live WK snapshot failed")
    })?;
    let _: () =
        unsafe { objc2::msg_send![webview, cacheDisplayInRect: frame, toBitmapImageRep: &*rep] };

    let pixels_wide: isize = unsafe { objc2::msg_send![&*rep, pixelsWide] };
    let pixels_high: isize = unsafe { objc2::msg_send![&*rep, pixelsHigh] };
    let samples: isize = unsafe { objc2::msg_send![&*rep, samplesPerPixel] };
    let bits_per_pixel: isize = unsafe { objc2::msg_send![&*rep, bitsPerPixel] };
    let row_bytes: isize = unsafe { objc2::msg_send![&*rep, bytesPerRow] };
    if pixels_wide <= 0
        || pixels_high <= 0
        || samples != 4
        || bits_per_pixel != 32
        || row_bytes < pixels_wide.saturating_mul(4)
    {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot bitmap is not packed 8-bit RGBA",
        ));
    }
    let out_w = u32::try_from(pixels_wide)
        .map_err(|_| HarnessError::backend_unavailable("live WK snapshot width overflow"))?;
    let out_h = u32::try_from(pixels_high)
        .map_err(|_| HarnessError::backend_unavailable("live WK snapshot height overflow"))?;
    if out_w > MAX_PIXEL_EDGE || out_h > MAX_PIXEL_EDGE {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot exceeds bounded raster edge",
        ));
    }
    let data: *mut u8 = unsafe { objc2::msg_send![&*rep, bitmapData] };
    if data.is_null() {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot bitmapData is null",
        ));
    }
    let packed_row = (out_w as usize).saturating_mul(4);
    let mut bytes = vec![0u8; packed_row.saturating_mul(out_h as usize)];
    let src_stride = row_bytes as usize;
    unsafe {
        for y in 0..out_h as usize {
            let src = data.add(y.saturating_mul(src_stride));
            let dst = y.saturating_mul(packed_row);
            bytes[dst..dst + packed_row]
                .copy_from_slice(std::slice::from_raw_parts(src, packed_row));
        }
    }
    Ok(NativeWkRaster {
        bytes,
        width: out_w,
        height: out_h,
    })
}

fn backing_scale(webview: &AnyObject) -> f64 {
    let responds: bool =
        unsafe { objc2::msg_send![webview, respondsToSelector: sel!(backingScaleFactor)] };
    if !responds {
        return 1.0;
    }
    let scale: f64 = unsafe { objc2::msg_send![webview, backingScaleFactor] };
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn clamp_edge(value: u32) -> u32 {
    value.clamp(1, MAX_PIXEL_EDGE)
}

fn is_main_thread() -> bool {
    let Some(cls) = AnyClass::get(c"NSThread") else {
        return false;
    };
    unsafe { objc2::msg_send![cls, isMainThread] }
}

fn webkit_loaded() -> bool {
    let path = c"/System/Library/Frameworks/WebKit.framework/WebKit";
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
    !handle.is_null()
}

fn nsstring(value: &str) -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"NSString")?;
    let cstr = CString::new(value).ok()?;
    unsafe { objc2::msg_send![cls, stringWithUTF8String: cstr.as_ptr()] }
}

fn pump_runloop_briefly() {
    let Some(cls) = AnyClass::get(c"NSRunLoop") else {
        return;
    };
    let current: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, currentRunLoop] };
    let Some(current) = current else {
        return;
    };
    let Some(date_cls) = AnyClass::get(c"NSDate") else {
        return;
    };
    let date: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![date_cls, dateWithTimeIntervalSinceNow: 0.05f64] };
    let Some(date) = date else {
        return;
    };
    let Some(mode) = default_run_loop_mode() else {
        return;
    };
    let _: bool = unsafe { objc2::msg_send![&*current, runMode: mode, beforeDate: &*date] };
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const AnyObject;
}

fn default_run_loop_mode() -> Option<&'static AnyObject> {
    let ptr = unsafe { kCFRunLoopDefaultMode };
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { &*ptr })
}

#[cfg(target_pointer_width = "64")]
type CGFloat = f64;
#[cfg(not(target_pointer_width = "64"))]
type CGFloat = f32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: CGFloat,
    y: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: CGFloat,
    height: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[CGFloat::ENCODING, CGFloat::ENCODING]);
}

unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[CGFloat::ENCODING, CGFloat::ENCODING]);
}

unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

fn cg_rect(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}
