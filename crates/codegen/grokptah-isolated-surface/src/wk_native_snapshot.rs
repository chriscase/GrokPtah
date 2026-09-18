//! Smallest honest native WKWebView raster for receipt-gated capture.
//!
//! Creates a real offscreen `WKWebView`, loads a tiny HTML fixture, and copies
//! RGBA8 from `takeSnapshotWithConfiguration:completionHandler:` (WK-composited
//! pixels, not `NSView` backing-store white). This is not isolation PASS and
//! never enables admission. Uniform window-white fail-closes.

use std::ffi::CString;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::encode::{Encode, Encoding};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::sel;

use crate::browser_engine_capture::LIVE_WK_FIXTURE_CRIMSON_RGB;
use crate::cb_containment::owned_page_for_boot;
use crate::error::{HarnessError, HarnessResult};

pub(crate) struct NativeWkRaster {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const FIXTURE_HTML: &str =
    "<!doctype html><html><body style=\"margin:0;background:#c41e3a\"></body></html>";
const LOGICAL_WIDTH: f64 = 16.0;
const LOGICAL_HEIGHT: f64 = 16.0;
const MAX_PIXEL_EDGE: u32 = 64;
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
const FIXTURE_CHANNEL_TOLERANCE: u16 = 40;

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
    let owned_page = owned_page_for_boot()?;

    let html = nsstring(FIXTURE_HTML).ok_or_else(|| {
        HarnessError::backend_unavailable("live WK snapshot HTML NSString unavailable")
    })?;
    let config_cls = AnyClass::get(c"WKWebViewConfiguration")
        .ok_or_else(|| HarnessError::backend_unavailable("WKWebViewConfiguration unavailable"))?;
    let config: Retained<AnyObject> = unsafe { objc2::msg_send![config_cls, new] };
    attach_nonpersistent_website_data_store(&config)?;

    let webview_cls = AnyClass::get(c"WKWebView")
        .ok_or_else(|| HarnessError::backend_unavailable("WKWebView unavailable"))?;
    let frame = cg_rect(0.0, 0.0, LOGICAL_WIDTH, LOGICAL_HEIGHT);
    let webview_alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![webview_cls, alloc] };
    let webview: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![webview_alloc, initWithFrame: frame, configuration: &*config] };
    let webview = webview
        .ok_or_else(|| HarnessError::backend_unavailable("WKWebView initWithFrame unavailable"))?;

    let _window = attach_offscreen_window(&webview, frame)?;

    let base_url = nsurl(owned_page).ok_or_else(|| {
        HarnessError::backend_unavailable("owned-page NSURL unavailable for live WK snapshot")
    })?;
    let _navigation: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![&*webview, loadHTMLString: &*html, baseURL: &*base_url] };
    wait_until_loaded(&webview)?;

    let _: () = unsafe { objc2::msg_send![&*webview, layoutSubtreeIfNeeded] };
    for _ in 0..8 {
        pump_runloop_briefly();
    }

    let raster = take_wk_snapshot(&webview, frame)?;
    if raster_is_unpainted_white(&raster.bytes) {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot is unpainted window-white, not WK-composited fixture pixels",
        ));
    }
    if !raster_contains_fixture_crimson(&raster.bytes) {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot does not contain fixture #c41e3a pixels",
        ));
    }
    Ok(raster)
}

pub(crate) fn raster_is_unpainted_white(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len().is_multiple_of(4)
        && bytes
            .chunks_exact(4)
            .all(|pixel| pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255)
}

pub(crate) fn raster_contains_fixture_crimson(bytes: &[u8]) -> bool {
    bytes.chunks_exact(4).any(pixel_near_fixture_crimson)
}

fn pixel_near_fixture_crimson(pixel: &[u8]) -> bool {
    let [target_r, target_g, target_b] = LIVE_WK_FIXTURE_CRIMSON_RGB;
    channel_near(pixel[0], target_r)
        && channel_near(pixel[1], target_g)
        && channel_near(pixel[2], target_b)
        || channel_near(pixel[0], target_b)
            && channel_near(pixel[1], target_g)
            && channel_near(pixel[2], target_r)
}

fn channel_near(actual: u8, target: u8) -> bool {
    (actual as i16 - target as i16).unsigned_abs() <= FIXTURE_CHANNEL_TOLERANCE
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

fn wait_until_loaded(webview: &AnyObject) -> HarnessResult<()> {
    let started = Instant::now();
    let mut saw_loading = false;
    loop {
        if started.elapsed() > LOAD_TIMEOUT {
            return Err(HarnessError::backend_unavailable(
                "live WK snapshot timed out waiting for WKWebView load",
            ));
        }
        let loading: bool = unsafe { objc2::msg_send![webview, isLoading] };
        if loading {
            saw_loading = true;
        }
        let progress: f64 = unsafe { objc2::msg_send![webview, estimatedProgress] };
        if (progress >= 1.0 || saw_loading) && !loading {
            pump_runloop_briefly();
            return Ok(());
        }
        if !saw_loading && !loading && started.elapsed() > Duration::from_millis(250) {
            pump_runloop_briefly();
            return Ok(());
        }
        pump_runloop_briefly();
    }
}

fn take_wk_snapshot(webview: &AnyObject, frame: CGRect) -> HarnessResult<NativeWkRaster> {
    let responds: bool = unsafe {
        objc2::msg_send![
            webview,
            respondsToSelector: sel!(takeSnapshotWithConfiguration:completionHandler:)
        ]
    };
    if !responds {
        return Err(HarnessError::backend_unavailable(
            "WKWebView takeSnapshotWithConfiguration:completionHandler: unavailable",
        ));
    }
    let snap_cls = AnyClass::get(c"WKSnapshotConfiguration").ok_or_else(|| {
        HarnessError::backend_unavailable("WKSnapshotConfiguration class unavailable")
    })?;
    let snap_cfg: Retained<AnyObject> = unsafe { objc2::msg_send![snap_cls, new] };
    let _: () = unsafe { objc2::msg_send![&*snap_cfg, setRect: frame] };
    let _: () = unsafe { objc2::msg_send![&*snap_cfg, setAfterScreenUpdates: true] };
    if let Some(num_cls) = AnyClass::get(c"NSNumber") {
        let width: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![num_cls, numberWithDouble: LOGICAL_WIDTH] };
        if let Some(width) = width {
            let _: () = unsafe { objc2::msg_send![&*snap_cfg, setSnapshotWidth: &*width] };
        }
    }

    let (tx, rx) = mpsc::sync_channel::<Result<Retained<AnyObject>, String>>(1);
    let block = RcBlock::new(move |image: *mut AnyObject, error: *mut AnyObject| {
        if image.is_null() {
            let message = nserror_message(error)
                .unwrap_or_else(|| "takeSnapshot returned a null NSImage".into());
            let _ = tx.send(Err(message));
            return;
        }
        match unsafe { Retained::retain(image) } {
            Some(image) => {
                let _ = tx.send(Ok(image));
            }
            None => {
                let _ = tx.send(Err("takeSnapshot NSImage retain failed".into()));
            }
        }
    });

    let _: () = unsafe {
        objc2::msg_send![
            webview,
            takeSnapshotWithConfiguration: &*snap_cfg,
            completionHandler: &*block
        ]
    };

    let started = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(Ok(image)) => return nsimage_to_rgba8(&image),
            Ok(Err(message)) => {
                return Err(HarnessError::backend_unavailable(format!(
                    "live WK takeSnapshot failed: {message}"
                )));
            }
            Err(mpsc::TryRecvError::Empty) => {
                if started.elapsed() > LOAD_TIMEOUT {
                    return Err(HarnessError::backend_unavailable(
                        "live WK takeSnapshot timed out",
                    ));
                }
                pump_runloop_briefly();
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(HarnessError::backend_unavailable(
                    "live WK takeSnapshot completion dropped",
                ));
            }
        }
    }
}

fn nsimage_to_rgba8(image: &AnyObject) -> HarnessResult<NativeWkRaster> {
    let tiff: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![image, TIFFRepresentation] };
    let tiff = tiff.ok_or_else(|| {
        HarnessError::backend_unavailable("live WK snapshot NSImage has no TIFFRepresentation")
    })?;
    let rep_cls = AnyClass::get(c"NSBitmapImageRep")
        .ok_or_else(|| HarnessError::backend_unavailable("NSBitmapImageRep unavailable"))?;
    let rep: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![rep_cls, imageRepWithData: &*tiff] };
    let rep = rep.ok_or_else(|| {
        HarnessError::backend_unavailable("live WK snapshot TIFF is not an NSBitmapImageRep")
    })?;
    bitmap_rep_to_rgba8(&rep)
}

fn bitmap_rep_to_rgba8(rep: &AnyObject) -> HarnessResult<NativeWkRaster> {
    let pixels_wide: isize = unsafe { objc2::msg_send![rep, pixelsWide] };
    let pixels_high: isize = unsafe { objc2::msg_send![rep, pixelsHigh] };
    let samples: isize = unsafe { objc2::msg_send![rep, samplesPerPixel] };
    let bits_per_pixel: isize = unsafe { objc2::msg_send![rep, bitsPerPixel] };
    let row_bytes: isize = unsafe { objc2::msg_send![rep, bytesPerRow] };
    if pixels_wide <= 0
        || pixels_high <= 0
        || samples < 3
        || bits_per_pixel < 24
        || row_bytes < pixels_wide.saturating_mul(samples)
    {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot bitmap is not packed 8-bit RGB/RGBA",
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
    let data: *mut u8 = unsafe { objc2::msg_send![rep, bitmapData] };
    if data.is_null() {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot bitmapData is null",
        ));
    }
    let spp = samples as usize;
    let packed_row = (out_w as usize).saturating_mul(4);
    let mut bytes = vec![0u8; packed_row.saturating_mul(out_h as usize)];
    let src_stride = row_bytes as usize;
    unsafe {
        for y in 0..out_h as usize {
            let src_row = data.add(y.saturating_mul(src_stride));
            for x in 0..out_w as usize {
                let src = std::slice::from_raw_parts(src_row.add(x.saturating_mul(spp)), spp);
                let dst = y.saturating_mul(packed_row) + x.saturating_mul(4);
                bytes[dst] = src[0];
                bytes[dst + 1] = src[1];
                bytes[dst + 2] = src[2];
                bytes[dst + 3] = if spp >= 4 { src[3] } else { 255 };
            }
        }
    }
    Ok(NativeWkRaster {
        bytes,
        width: out_w,
        height: out_h,
    })
}

fn nserror_message(error: *mut AnyObject) -> Option<String> {
    if error.is_null() {
        return None;
    }
    let desc: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![&*error, localizedDescription] };
    nsstring_to_string(desc.as_deref())
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

fn attach_nonpersistent_website_data_store(config: &AnyObject) -> HarnessResult<()> {
    let Some(store_cls) = AnyClass::get(c"WKWebsiteDataStore") else {
        return Err(HarnessError::backend_unavailable(
            "WKWebsiteDataStore unavailable for nonpersistent store",
        ));
    };
    let store: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![store_cls, nonPersistentDataStore] };
    let store = store.ok_or_else(|| {
        HarnessError::backend_unavailable("WKWebsiteDataStore.nonPersistentDataStore unavailable")
    })?;
    let _: () = unsafe { objc2::msg_send![config, setWebsiteDataStore: &*store] };
    Ok(())
}

fn nsurl(value: &str) -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"NSURL")?;
    let string = nsstring(value)?;
    unsafe { objc2::msg_send![cls, URLWithString: &*string] }
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
