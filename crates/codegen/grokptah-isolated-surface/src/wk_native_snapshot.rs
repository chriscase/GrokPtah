//! Smallest honest native WKWebView raster for receipt-gated capture.
//!
//! Creates a real offscreen `WKWebView`, loads a tiny HTML fixture, and copies
//! RGBA8 from `takeSnapshotWithConfiguration:completionHandler:` (WK-composited
//! pixels, not `NSView` backing-store white). This is not isolation PASS and
//! never enables admission. Uniform window-white fail-closes.

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::encode::{Encode, Encoding};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, NSObject, Sel};
use objc2::{sel, ClassType};

use crate::browser_engine_capture::{LIVE_WK_FIXTURE_CLICKED_RGB, LIVE_WK_FIXTURE_CRIMSON_RGB};
use crate::cb_containment::{
    admit_navigation, live_wk_create_webview_policy_allows, live_wk_download_policy_allows,
    live_wk_navigation_action_policy_allows, live_wk_navigation_response_policy_allows,
    live_wk_open_panel_policy_allows, owned_page_for_boot, NativeDenyKind,
};
use crate::error::{HarnessError, HarnessResult};
use crate::simulator::GuestLocalAction;

pub(crate) struct NativeWkRaster {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const FIXTURE_HTML: &str = concat!(
    "<!doctype html><html><body id=\"cb-v0-root\" ",
    "style=\"margin:0;background:#c41e3a;width:100vw;height:100vh\">",
    "<div id=\"cb-v0-probes\" style=\"display:none\">",
    "<a id=\"cb-v0-blank\" href=\"https://evil.example/\" target=\"_blank\" rel=\"noopener\">blank</a>",
    "<a id=\"cb-v0-download\" href=\"https://grokptah.owned.invalid/cb-v0/deny.bin\" download=\"deny.bin\">dl</a>",
    "<input id=\"cb-v0-file\" type=\"file\">",
    "<input id=\"cb-v0-dir\" type=\"file\" webkitdirectory>",
    "<button id=\"cb-v0-popup\" type=\"button\">popup</button>",
    "</div>",
    "<script>",
    "document.getElementById('cb-v0-root').addEventListener('click',function(){",
    "this.style.background='#1a6b3c';",
    "this.setAttribute('data-grokptah-clicked','1');",
    "});",
    "</script></body></html>"
);

const WINDOW_OPEN_JS: &str = concat!(
    "(function(){var w=window.open('https://evil.example/','_blank');",
    "if(w){try{w.close();}catch(e){}",
    "return 'opened';}return 'null';})()"
);

const POPUP_OPEN_JS: &str = concat!(
    "(function(){var w=window.open('https://evil.example/','cbv0popup','width=80,height=80');",
    "if(w){try{w.close();}catch(e){}",
    "return 'opened';}return 'null';})()"
);

const DOWNLOAD_CLICK_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-download');",
    "if(!el){return 'missing';}el.click();return 'clicked';})()"
);

const BLANK_CLICK_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-blank');",
    "if(!el){return 'missing';}el.click();return 'clicked';})()"
);

const FILE_CLICK_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-file');",
    "if(!el){return 'missing';}el.click();return 'clicked';})()"
);

const DIR_CLICK_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-dir');",
    "if(!el){return 'missing';}el.click();return 'clicked';})()"
);

const CLICK_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-root')||document.body;",
    "el.dispatchEvent(new MouseEvent('click',{bubbles:true,cancelable:true,view:window}));",
    "return el.getAttribute('data-grokptah-clicked')||'';})()"
);

const TYPE_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-root')||document.body;",
    "el.style.background='#1a6b3c';",
    "el.setAttribute('data-grokptah-typed','1');",
    "return el.getAttribute('data-grokptah-typed')||'';})()"
);

/// Process-local live WK session: one owned page, main-frame DOM mutate, then snapshot.
pub(crate) struct LiveWkSession {
    webview: Retained<AnyObject>,
    _window: Option<Retained<AnyObject>>,
    _delegate: Retained<AnyObject>,
    store: Retained<AnyObject>,
}

impl std::fmt::Debug for LiveWkSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveWkSession").finish_non_exhaustive()
    }
}
const LOGICAL_WIDTH: f64 = 16.0;
const LOGICAL_HEIGHT: f64 = 16.0;
const MAX_PIXEL_EDGE: u32 = 64;
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
const FIXTURE_CHANNEL_TOLERANCE: u16 = 40;

pub(crate) fn rasterize_fixture() -> HarnessResult<NativeWkRaster> {
    let session = LiveWkSession::open()?;
    let raster = session.snapshot()?;
    if !raster_contains_fixture_crimson(&raster.bytes) {
        return Err(HarnessError::backend_unavailable(
            "live WK snapshot does not contain fixture #c41e3a pixels",
        ));
    }
    Ok(raster)
}

impl LiveWkSession {
    pub(crate) fn open() -> HarnessResult<Self> {
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
        ensure_ns_application()?;
        let owned_page = owned_page_for_boot()?;

        let html = nsstring(FIXTURE_HTML).ok_or_else(|| {
            HarnessError::backend_unavailable("live WK snapshot HTML NSString unavailable")
        })?;
        let config_cls = AnyClass::get(c"WKWebViewConfiguration").ok_or_else(|| {
            HarnessError::backend_unavailable("WKWebViewConfiguration unavailable")
        })?;
        let config: Retained<AnyObject> = unsafe { objc2::msg_send![config_cls, new] };
        let store = attach_nonpersistent_website_data_store(&config)?;
        enable_javascript_window_open_asks_delegate(&config)?;

        let webview_cls = AnyClass::get(c"WKWebView")
            .ok_or_else(|| HarnessError::backend_unavailable("WKWebView unavailable"))?;
        let frame = cg_rect(0.0, 0.0, LOGICAL_WIDTH, LOGICAL_HEIGHT);
        let webview_alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![webview_cls, alloc] };
        let webview: Option<Retained<AnyObject>> = unsafe {
            objc2::msg_send![webview_alloc, initWithFrame: frame, configuration: &*config]
        };
        let webview = webview.ok_or_else(|| {
            HarnessError::backend_unavailable("WKWebView initWithFrame unavailable")
        })?;

        let delegate = containment_delegate_instance()?;
        let _: () = unsafe { objc2::msg_send![&*webview, setNavigationDelegate: &*delegate] };
        let _: () = unsafe { objc2::msg_send![&*webview, setUIDelegate: &*delegate] };

        let window = attach_offscreen_window(&webview, frame)?;
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
        Ok(Self {
            webview,
            _window: window,
            _delegate: delegate,
            store,
        })
    }

    pub(crate) fn snapshot(&self) -> HarnessResult<NativeWkRaster> {
        if !is_main_thread() {
            return Err(HarnessError::backend_unavailable(
                "live WK snapshot requires the process main thread",
            ));
        }
        let frame = cg_rect(0.0, 0.0, LOGICAL_WIDTH, LOGICAL_HEIGHT);
        let _: () = unsafe { objc2::msg_send![&*self.webview, layoutSubtreeIfNeeded] };
        for _ in 0..4 {
            pump_runloop_briefly();
        }
        let raster = take_wk_snapshot(&self.webview, frame)?;
        if raster_is_unpainted_white(&raster.bytes) {
            return Err(HarnessError::backend_unavailable(
                "live WK snapshot is unpainted window-white, not WK-composited fixture pixels",
            ));
        }
        Ok(raster)
    }

    pub(crate) fn mutate_main_frame_dom(&self, action: GuestLocalAction) -> HarnessResult<()> {
        if !is_main_thread() {
            return Err(HarnessError::backend_unavailable(
                "live WK DOM mutate requires the process main thread",
            ));
        }
        let js = match action {
            GuestLocalAction::ClickGuestButton => CLICK_JS,
            GuestLocalAction::TypeGuestText => TYPE_JS,
        };
        evaluate_javascript(&self.webview, js)?;
        let _: () = unsafe { objc2::msg_send![&*self.webview, layoutSubtreeIfNeeded] };
        for _ in 0..8 {
            pump_runloop_briefly();
        }
        Ok(())
    }

    pub(crate) fn website_data_store_is_persistent(&self) -> HarnessResult<bool> {
        let persistent: bool = unsafe { objc2::msg_send![&*self.store, isPersistent] };
        Ok(persistent)
    }

    pub(crate) fn current_url(&self) -> HarnessResult<String> {
        let url: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*self.webview, URL] };
        let url = url
            .ok_or_else(|| HarnessError::backend_unavailable("live WK webview URL unavailable"))?;
        let abs: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*url, absoluteString] };
        nsstring_to_string(abs.as_deref()).ok_or_else(|| {
            HarnessError::backend_unavailable("live WK webview URL string unavailable")
        })
    }

    fn webview_key(&self) -> usize {
        (&*self.webview) as *const AnyObject as usize
    }

    pub(crate) fn last_navigation_decision(&self) -> Option<(String, isize)> {
        last_recorded_navigation_decision(self.webview_key())
    }

    /// Load `url` through WK and wait for the navigation-delegate decision.
    /// Off-allowlist success is: policy cancelled **and** `current_url` still owned.
    /// Does not re-deny the requested URL in Rust.
    pub(crate) fn attempt_navigation(&self, url: &str) -> HarnessResult<()> {
        if !is_main_thread() {
            return Err(HarnessError::backend_unavailable(
                "live WK navigation requires the process main thread",
            ));
        }
        let key = self.webview_key();
        clear_navigation_decisions(key);
        let request_url = nsurl(url).ok_or_else(|| {
            HarnessError::backend_unavailable("live WK navigation NSURL unavailable")
        })?;
        let req_cls = AnyClass::get(c"NSURLRequest")
            .ok_or_else(|| HarnessError::backend_unavailable("NSURLRequest unavailable"))?;
        let request: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![req_cls, requestWithURL: &*request_url] };
        let request = request.ok_or_else(|| {
            HarnessError::backend_unavailable("NSURLRequest.requestWithURL unavailable")
        })?;
        let _nav: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![&*self.webview, loadRequest: &*request] };
        let decision = wait_for_navigation_decision(key, url)?;
        let current = self.current_url()?;
        if decision.1 != 0 {
            return Err(HarnessError::invalid_state(format!(
                "WK navigation policy allowed {url} (current={current})"
            )));
        }
        if admit_navigation(&current).is_err() {
            return Err(HarnessError::invalid_state(format!(
                "WK navigation policy leaked off-allowlist URL {current}"
            )));
        }
        Ok(())
    }

    pub(crate) fn website_data_store_object_key(&self) -> usize {
        (&*self.store) as *const AnyObject as usize
    }

    pub(crate) fn uses_default_website_data_store(&self) -> bool {
        default_website_data_store_key() == Some(self.website_data_store_object_key())
    }

    pub(crate) fn last_native_deny(&self) -> Option<NativeDenyKind> {
        last_recorded_native_deny(self.webview_key())
    }

    /// `window.open(_blank)` through page JS. Fail-closed unless WK consults
    /// `WKUIDelegate.createWebView...` and that IMP returns nil. Popup-blocked
    /// JS `null` without a createWebView callback is not a deny.
    pub(crate) fn attempt_window_open(&self) -> HarnessResult<()> {
        self.attempt_create_webview_js(WINDOW_OPEN_JS, NativeDenyKind::WindowOpen)
    }

    /// Popup `window.open` with features. Same nil `createWebView` deny, recorded
    /// as `Popup` from `WKWindowFeatures` width/height — not WindowOpen/NewWindow.
    pub(crate) fn attempt_popup(&self) -> HarnessResult<()> {
        self.attempt_create_webview_js(POPUP_OPEN_JS, NativeDenyKind::Popup)
    }

    /// `_blank` anchor click. WK `decidePolicyForNavigationAction` must cancel
    /// (`targetFrame == nil`) and record `NewWindow`. Not a createWebView poke.
    pub(crate) fn attempt_blank_target(&self) -> HarnessResult<()> {
        require_main_thread("live WK _blank deny")?;
        let key = self.webview_key();
        clear_native_denies(key);
        clear_navigation_decisions(key);
        let result = evaluate_javascript_value(&self.webview, BLANK_CLICK_JS)?;
        if result.as_deref() == Some("missing") {
            return Err(HarnessError::backend_unavailable(
                "owned-page _blank probe anchor is missing",
            ));
        }
        wait_for_native_deny(key, NativeDenyKind::NewWindow)?;
        let decision = wait_for_navigation_decision(key, "https://evil.example/")?;
        if decision.1 != 0 {
            return Err(HarnessError::invalid_state(format!(
                "WK _blank navigation policy allowed {} (policy={})",
                decision.0, decision.1
            )));
        }
        self.assert_still_owned("new_window")
    }

    /// `<a download href=".../deny.bin">` click. WK must consult navigation
    /// policy with `shouldPerformDownload` (or a download MIME response) and
    /// cancel. Dummy `didBecomeDownload` IMP pokes are not a deny.
    pub(crate) fn attempt_download(&self) -> HarnessResult<()> {
        require_main_thread("live WK download deny")?;
        let key = self.webview_key();
        clear_native_denies(key);
        clear_navigation_decisions(key);
        let result = evaluate_javascript_value(&self.webview, DOWNLOAD_CLICK_JS)?;
        if result.as_deref() == Some("missing") {
            return Err(HarnessError::backend_unavailable(
                "owned-page download probe anchor is missing",
            ));
        }
        wait_for_native_deny(key, NativeDenyKind::Download)?;
        let decision =
            wait_for_navigation_decision(key, "https://grokptah.owned.invalid/cb-v0/deny.bin")?;
        if decision.1 != 0 {
            return Err(HarnessError::invalid_state(format!(
                "WK download navigation policy allowed {} (policy={})",
                decision.0, decision.1
            )));
        }
        self.assert_still_owned("download")
    }

    /// File picker: WK does not deliver `runOpenPanel` without a real user
    /// gesture. This probe messages the attached `WKUIDelegate` (the same
    /// object WK holds). It is not a WK-originated open-panel oracle.
    /// Missing delegate or non-nil URLs fail closed.
    pub(crate) fn attempt_file_picker(&self) -> HarnessResult<()> {
        self.invoke_open_panel_on_attached_delegate(false)
    }

    /// Directory picker: same `runOpenPanel` deny with `allowsDirectories`.
    pub(crate) fn attempt_directory_picker(&self) -> HarnessResult<()> {
        self.invoke_open_panel_on_attached_delegate(true)
    }

    pub(crate) fn write_local_storage(&self, key: &str, value: &str) -> HarnessResult<()> {
        require_main_thread("live WK localStorage write")?;
        let js = format!(
            "(function(){{localStorage.setItem({k},{v});return localStorage.getItem({k})||'';}})()",
            k = js_string_literal(key),
            v = js_string_literal(value),
        );
        let wrote = evaluate_javascript_value(&self.webview, &js)?;
        if wrote.as_deref() != Some(value) {
            return Err(HarnessError::backend_unavailable(format!(
                "live WK localStorage write did not stick: {wrote:?}"
            )));
        }
        Ok(())
    }

    pub(crate) fn read_local_storage(&self, key: &str) -> HarnessResult<Option<String>> {
        require_main_thread("live WK localStorage read")?;
        let js = format!(
            "(function(){{var v=localStorage.getItem({k});return v===null?'':v;}})()",
            k = js_string_literal(key),
        );
        let value = evaluate_javascript_value(&self.webview, &js)?;
        Ok(value.filter(|s| !s.is_empty()))
    }

    fn assert_still_owned(&self, what: &str) -> HarnessResult<()> {
        let current = self.current_url()?;
        if admit_navigation(&current).is_err() {
            return Err(HarnessError::invalid_state(format!(
                "{what} leaked off-allowlist URL {current}"
            )));
        }
        Ok(())
    }

    fn attempt_create_webview_js(&self, js: &str, kind: NativeDenyKind) -> HarnessResult<()> {
        require_main_thread("live WK createWebView deny")?;
        let key = self.webview_key();
        clear_native_denies(key);
        let result = evaluate_javascript_value(&self.webview, js)?;
        if result.as_deref() == Some("opened") {
            return Err(HarnessError::invalid_state(format!(
                "WK createWebView leaked a window ({})",
                kind.as_str()
            )));
        }
        if result.as_deref() == Some("missing") {
            return Err(HarnessError::backend_unavailable(format!(
                "owned-page {} probe element is missing",
                kind.as_str()
            )));
        }
        wait_for_native_deny(key, kind).map_err(|err| {
            HarnessError::backend_unavailable(format!(
                "WK did not consult createWebView for {} (JS null without UIDelegate is not a deny): {err}",
                kind.as_str()
            ))
        })?;
        self.assert_still_owned(kind.as_str())
    }

    fn invoke_open_panel_on_attached_delegate(
        &self,
        allows_directories: bool,
    ) -> HarnessResult<()> {
        require_main_thread("live WK open-panel deny")?;
        let kind = if allows_directories {
            NativeDenyKind::DirectoryPicker
        } else {
            NativeDenyKind::FilePicker
        };
        let key = self.webview_key();
        clear_native_denies(key);
        clear_open_panel_completions(key);
        let _ = evaluate_javascript_value(
            &self.webview,
            if allows_directories {
                DIR_CLICK_JS
            } else {
                FILE_CLICK_JS
            },
        )?;
        if wait_for_native_deny_until(key, kind, Duration::from_millis(250)).is_ok() {
            wait_for_open_panel_nil(key)?;
            return self.assert_still_owned(kind.as_str());
        }
        let delegate = attached_ui_delegate(&self.webview)?;
        let params = probe_open_panel_parameters(allows_directories)?;
        let (tx, rx) = mpsc::sync_channel::<bool>(1);
        let block = RcBlock::new(move |urls: *mut AnyObject| {
            let _ = tx.send(urls.is_null());
        });
        let _: () = unsafe {
            objc2::msg_send![
                &*delegate,
                webView: &*self.webview,
                runOpenPanelWithParameters: &*params,
                initiatedByFrame: &*self.webview,
                completionHandler: &*block
            ]
        };
        let started = Instant::now();
        let urls_null = loop {
            match rx.try_recv() {
                Ok(urls_null) => break urls_null,
                Err(mpsc::TryRecvError::Empty) => {
                    if started.elapsed() > LOAD_TIMEOUT {
                        return Err(HarnessError::backend_unavailable(
                            "attached UIDelegate runOpenPanel did not invoke completionHandler",
                        ));
                    }
                    pump_runloop_briefly();
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(HarnessError::backend_unavailable(
                        "attached UIDelegate runOpenPanel completion dropped",
                    ));
                }
            }
        };
        if !urls_null {
            return Err(HarnessError::invalid_state(
                "runOpenPanel completionHandler received URLs; file/directory picker must deny with nil",
            ));
        }
        wait_for_native_deny(key, kind)?;
        self.assert_still_owned(kind.as_str())
    }
}

fn raster_contains_fixture_clicked(bytes: &[u8]) -> bool {
    bytes.chunks_exact(4).any(pixel_near_fixture_clicked)
}

fn pixel_near_fixture_clicked(pixel: &[u8]) -> bool {
    let [target_r, target_g, target_b] = LIVE_WK_FIXTURE_CLICKED_RGB;
    channel_near(pixel[0], target_r)
        && channel_near(pixel[1], target_g)
        && channel_near(pixel[2], target_b)
        || channel_near(pixel[0], target_b)
            && channel_near(pixel[1], target_g)
            && channel_near(pixel[2], target_r)
}

pub(crate) fn raster_contains_clicked_pixels(bytes: &[u8]) -> bool {
    raster_contains_fixture_clicked(bytes)
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

fn require_main_thread(what: &str) -> HarnessResult<()> {
    if !is_main_thread() {
        return Err(HarnessError::backend_unavailable(format!(
            "{what} requires the process main thread"
        )));
    }
    Ok(())
}

fn webkit_loaded() -> bool {
    let path = c"/System/Library/Frameworks/WebKit.framework/WebKit";
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
    !handle.is_null()
}

fn attach_nonpersistent_website_data_store(
    config: &AnyObject,
) -> HarnessResult<Retained<AnyObject>> {
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
    let persistent: bool = unsafe { objc2::msg_send![&*store, isPersistent] };
    if persistent {
        return Err(HarnessError::backend_unavailable(
            "WKWebsiteDataStore.nonPersistentDataStore reported isPersistent == true",
        ));
    }
    if default_website_data_store_key() == Some((&*store) as *const AnyObject as usize) {
        return Err(HarnessError::backend_unavailable(
            "live WK must not attach the default/profile website-data store",
        ));
    }
    let _: () = unsafe { objc2::msg_send![config, setWebsiteDataStore: &*store] };
    Ok(store)
}

fn default_website_data_store_key() -> Option<usize> {
    let cls = AnyClass::get(c"WKWebsiteDataStore")?;
    let store: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, defaultDataStore] };
    store.map(|store| (&*store) as *const AnyObject as usize)
}

fn enable_javascript_window_open_asks_delegate(config: &AnyObject) -> HarnessResult<()> {
    let prefs: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![config, preferences] };
    let prefs = prefs.ok_or_else(|| {
        HarnessError::backend_unavailable("WKPreferences unavailable for window-open deny")
    })?;
    // Ask the UIDelegate instead of silently returning null without a callback.
    let _: () =
        unsafe { objc2::msg_send![&*prefs, setJavaScriptCanOpenWindowsAutomatically: Bool::YES] };
    Ok(())
}

fn evaluate_javascript(webview: &AnyObject, js: &str) -> HarnessResult<()> {
    evaluate_javascript_value(webview, js).map(|_| ())
}

fn evaluate_javascript_value(webview: &AnyObject, js: &str) -> HarnessResult<Option<String>> {
    let script = nsstring(js).ok_or_else(|| {
        HarnessError::backend_unavailable("live WK evaluateJavaScript NSString unavailable")
    })?;
    let (tx, rx) = mpsc::sync_channel::<Result<Option<String>, String>>(1);
    let block = RcBlock::new(move |value: *mut AnyObject, error: *mut AnyObject| {
        if !error.is_null() {
            let message = nserror_message(error)
                .unwrap_or_else(|| "evaluateJavaScript returned an error".into());
            let _ = tx.send(Err(message));
            return;
        }
        let _ = tx.send(Ok(js_value_to_optional_string(value)));
    });
    let _: () = unsafe {
        objc2::msg_send![
            webview,
            evaluateJavaScript: &*script,
            completionHandler: &*block
        ]
    };
    let started = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(message)) => {
                return Err(HarnessError::backend_unavailable(format!(
                    "live WK JavaScript failed: {message}"
                )));
            }
            Err(mpsc::TryRecvError::Empty) => {
                if started.elapsed() > LOAD_TIMEOUT {
                    return Err(HarnessError::backend_unavailable(
                        "live WK evaluateJavaScript timed out",
                    ));
                }
                pump_runloop_briefly();
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(HarnessError::backend_unavailable(
                    "live WK evaluateJavaScript completion dropped",
                ));
            }
        }
    }
}

fn js_value_to_optional_string(value: *mut AnyObject) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let value = unsafe { &*value };
    if let Some(nsnull) = AnyClass::get(c"NSNull") {
        let is_null: bool = unsafe { objc2::msg_send![value, isKindOfClass: nsnull] };
        if is_null {
            return None;
        }
    }
    nsstring_to_string(Some(value)).or_else(|| {
        let desc: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![value, description] };
        nsstring_to_string(desc.as_deref())
    })
}

fn js_string_literal(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

fn containment_delegate_class() -> Option<&'static AnyClass> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    *CLASS.get_or_init(|| {
        if let Some(existing) = AnyClass::get(c"GrokptahCbV0ContainmentDelegate") {
            return Some(existing);
        }
        let mut builder = ClassBuilder::new(c"GrokptahCbV0ContainmentDelegate", NSObject::class())?;
        if let Some(protocol) = AnyProtocol::get(c"WKNavigationDelegate") {
            let _ = builder.add_protocol(protocol);
        }
        if let Some(protocol) = AnyProtocol::get(c"WKUIDelegate") {
            let _ = builder.add_protocol(protocol);
        }
        if let Some(protocol) = AnyProtocol::get(c"WKDownloadDelegate") {
            let _ = builder.add_protocol(protocol);
        }
        unsafe {
            builder.add_method(
                sel!(webView:decidePolicyForNavigationAction:decisionHandler:),
                decide_navigation_policy as unsafe extern "C-unwind" fn(_, _, _, _, _),
            );
            builder.add_method(
                sel!(webView:decidePolicyForNavigationResponse:decisionHandler:),
                decide_navigation_response as unsafe extern "C-unwind" fn(_, _, _, _, _),
            );
            builder.add_method(
                sel!(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:),
                create_webview as unsafe extern "C-unwind" fn(_, _, _, _, _, _) -> *mut AnyObject,
            );
            builder.add_method(
                sel!(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:),
                run_open_panel as unsafe extern "C-unwind" fn(_, _, _, _, _, _),
            );
            builder.add_method(
                sel!(webView:navigationAction:didBecomeDownload:),
                navigation_action_became_download as unsafe extern "C-unwind" fn(_, _, _, _, _),
            );
            builder.add_method(
                sel!(webView:navigationResponse:didBecomeDownload:),
                navigation_response_became_download as unsafe extern "C-unwind" fn(_, _, _, _, _),
            );
            builder.add_method(
                sel!(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:),
                download_decide_destination as unsafe extern "C-unwind" fn(_, _, _, _, _, _),
            );
        }
        Some(builder.register())
    })
}

fn containment_delegate_instance() -> HarnessResult<Retained<AnyObject>> {
    let cls = containment_delegate_class().ok_or_else(|| {
        HarnessError::backend_unavailable("WK containment deny-delegate class unavailable")
    })?;
    let obj: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, new] };
    obj.ok_or_else(|| {
        HarnessError::backend_unavailable("WK containment deny-delegate alloc failed")
    })
}

type NavigationDecisionLog = HashMap<usize, Vec<(String, isize)>>;

fn navigation_decision_log() -> &'static Mutex<NavigationDecisionLog> {
    static LOG: OnceLock<Mutex<NavigationDecisionLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_navigation_decision(key: usize, url: String, policy: isize) {
    if let Ok(mut guard) = navigation_decision_log().lock() {
        guard.entry(key).or_default().push((url, policy));
    }
}

fn clear_navigation_decisions(key: usize) {
    if let Ok(mut guard) = navigation_decision_log().lock() {
        guard.insert(key, Vec::new());
    }
}

fn last_recorded_navigation_decision(key: usize) -> Option<(String, isize)> {
    navigation_decision_log()
        .lock()
        .ok()?
        .get(&key)?
        .last()
        .cloned()
}

fn wait_for_navigation_decision(key: usize, needle: &str) -> HarnessResult<(String, isize)> {
    let started = Instant::now();
    loop {
        if let Ok(guard) = navigation_decision_log().lock() {
            if let Some(list) = guard.get(&key) {
                if let Some(hit) = list.iter().rev().find(|(url, _)| {
                    url == needle || url.starts_with(needle) || needle.starts_with(url)
                }) {
                    return Ok(hit.clone());
                }
            }
        }
        if started.elapsed() > LOAD_TIMEOUT {
            return Err(HarnessError::backend_unavailable(format!(
                "WK navigation delegate did not decide for {needle}"
            )));
        }
        pump_runloop_briefly();
    }
}

type NativeDenyLog = HashMap<usize, Vec<NativeDenyKind>>;

fn native_deny_log() -> &'static Mutex<NativeDenyLog> {
    static LOG: OnceLock<Mutex<NativeDenyLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_native_deny(key: usize, kind: NativeDenyKind) {
    if let Ok(mut guard) = native_deny_log().lock() {
        guard.entry(key).or_default().push(kind);
    }
}

fn clear_native_denies(key: usize) {
    if let Ok(mut guard) = native_deny_log().lock() {
        guard.insert(key, Vec::new());
    }
}

fn recorded_native_denies(key: usize) -> Vec<NativeDenyKind> {
    native_deny_log()
        .lock()
        .ok()
        .and_then(|guard| guard.get(&key).cloned())
        .unwrap_or_default()
}

fn last_recorded_native_deny(key: usize) -> Option<NativeDenyKind> {
    recorded_native_denies(key).last().copied()
}

fn wait_for_native_deny(key: usize, kind: NativeDenyKind) -> HarnessResult<()> {
    wait_for_native_deny_until(key, kind, LOAD_TIMEOUT)
}

fn wait_for_native_deny_until(
    key: usize,
    kind: NativeDenyKind,
    timeout: Duration,
) -> HarnessResult<()> {
    let started = Instant::now();
    loop {
        if recorded_native_denies(key).contains(&kind) {
            return Ok(());
        }
        if started.elapsed() > timeout {
            return Err(HarnessError::backend_unavailable(format!(
                "WK native deny delegate did not fire for {}",
                kind.as_str()
            )));
        }
        pump_runloop_briefly();
    }
}

type OpenPanelCompletionLog = HashMap<usize, Vec<bool>>;

fn open_panel_completion_log() -> &'static Mutex<OpenPanelCompletionLog> {
    static LOG: OnceLock<Mutex<OpenPanelCompletionLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_open_panel_completion(key: usize, urls_null: bool) {
    if let Ok(mut guard) = open_panel_completion_log().lock() {
        guard.entry(key).or_default().push(urls_null);
    }
}

fn clear_open_panel_completions(key: usize) {
    if let Ok(mut guard) = open_panel_completion_log().lock() {
        guard.insert(key, Vec::new());
    }
}

fn wait_for_open_panel_nil(key: usize) -> HarnessResult<()> {
    let started = Instant::now();
    loop {
        if let Ok(guard) = open_panel_completion_log().lock() {
            if guard
                .get(&key)
                .is_some_and(|list| list.iter().any(|nil| *nil))
            {
                return Ok(());
            }
            if guard
                .get(&key)
                .is_some_and(|list| list.iter().any(|nil| !*nil))
            {
                return Err(HarnessError::invalid_state(
                    "runOpenPanel completionHandler received URLs; picker must deny with nil",
                ));
            }
        }
        if started.elapsed() > LOAD_TIMEOUT {
            return Err(HarnessError::backend_unavailable(
                "WK runOpenPanel completionHandler did not fire",
            ));
        }
        pump_runloop_briefly();
    }
}

unsafe extern "C-unwind" fn decide_navigation_policy(
    _this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    action: &AnyObject,
    decision_handler: *mut std::ffi::c_void,
) {
    let url = navigation_action_url(action).unwrap_or_default();
    let should_download = navigation_action_should_download(action);
    let is_blank = navigation_action_is_blank(action);
    let policy = navigation_action_policy(action);
    if policy == 0 {
        if should_download && !live_wk_download_policy_allows() {
            record_native_deny(webview as usize, NativeDenyKind::Download);
        } else if is_blank && !live_wk_create_webview_policy_allows() {
            record_native_deny(webview as usize, NativeDenyKind::NewWindow);
        }
    }
    record_navigation_decision(webview as usize, url, policy);
    invoke_navigation_decision(decision_handler, policy);
}

unsafe extern "C-unwind" fn decide_navigation_response(
    _this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    response: &AnyObject,
    decision_handler: *mut std::ffi::c_void,
) {
    let url = navigation_response_url(response).unwrap_or_default();
    let is_main: bool = unsafe { objc2::msg_send![response, isForMainFrame] };
    let can_show: bool = unsafe { objc2::msg_send![response, canShowMIMEType] };
    let is_download = navigation_response_is_download(response);
    let allow = live_wk_navigation_response_policy_allows(&url, is_main, can_show, is_download);
    let policy = if allow { 1 } else { 0 };
    if !allow && (is_download || !can_show) && !live_wk_download_policy_allows() {
        record_native_deny(webview as usize, NativeDenyKind::Download);
    }
    invoke_navigation_decision(decision_handler, policy);
}

unsafe extern "C-unwind" fn create_webview(
    _this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    _config: *mut AnyObject,
    _action: *mut AnyObject,
    features: *mut AnyObject,
) -> *mut AnyObject {
    let key = webview as usize;
    if !live_wk_create_webview_policy_allows() {
        record_native_deny(key, create_webview_deny_kind(features));
    }
    std::ptr::null_mut()
}

fn create_webview_deny_kind(features: *mut AnyObject) -> NativeDenyKind {
    if window_features_indicate_popup(features) {
        NativeDenyKind::Popup
    } else {
        NativeDenyKind::WindowOpen
    }
}

fn window_features_indicate_popup(features: *mut AnyObject) -> bool {
    if features.is_null() {
        return false;
    }
    let features = unsafe { &*features };
    let width: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![features, width] };
    let height: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![features, height] };
    width.is_some() || height.is_some()
}

unsafe extern "C-unwind" fn run_open_panel(
    _this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    params: *mut AnyObject,
    _frame: *mut AnyObject,
    decision_handler: *mut std::ffi::c_void,
) {
    let allows_directories = open_panel_allows_directories(params);
    let kind = if allows_directories {
        NativeDenyKind::DirectoryPicker
    } else {
        NativeDenyKind::FilePicker
    };
    if !live_wk_open_panel_policy_allows(allows_directories) {
        record_native_deny(webview as usize, kind);
        record_open_panel_completion(webview as usize, true);
    }
    invoke_object_completion(decision_handler, std::ptr::null_mut());
}

unsafe extern "C-unwind" fn navigation_action_became_download(
    this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    _action: *mut AnyObject,
    download: *mut AnyObject,
) {
    if !live_wk_download_policy_allows() {
        record_native_deny(webview as usize, NativeDenyKind::Download);
        cancel_wk_download(this, download);
    }
}

unsafe extern "C-unwind" fn navigation_response_became_download(
    this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    _response: *mut AnyObject,
    download: *mut AnyObject,
) {
    if !live_wk_download_policy_allows() {
        record_native_deny(webview as usize, NativeDenyKind::Download);
        cancel_wk_download(this, download);
    }
}

unsafe extern "C-unwind" fn download_decide_destination(
    _this: &AnyObject,
    _cmd: Sel,
    _download: *mut AnyObject,
    _response: *mut AnyObject,
    _filename: *mut AnyObject,
    decision_handler: *mut std::ffi::c_void,
) {
    let _allow = live_wk_download_policy_allows();
    invoke_object_completion(decision_handler, std::ptr::null_mut());
}

fn cancel_wk_download(delegate: &AnyObject, download: *mut AnyObject) {
    if download.is_null() {
        return;
    }
    let download = unsafe { &*download };
    let set_delegate: bool =
        unsafe { objc2::msg_send![download, respondsToSelector: sel!(setDelegate:)] };
    if set_delegate {
        let _: () = unsafe { objc2::msg_send![download, setDelegate: delegate] };
    }
    let can_cancel: bool = unsafe { objc2::msg_send![download, respondsToSelector: sel!(cancel)] };
    if can_cancel {
        let _: () = unsafe { objc2::msg_send![download, cancel] };
    }
}

fn navigation_action_policy(action: &AnyObject) -> isize {
    let is_blank = navigation_action_is_blank(action);
    let is_main = navigation_action_is_main_frame(action);
    let url = navigation_action_url(action).unwrap_or_default();
    let should_download = navigation_action_should_download(action);
    if live_wk_navigation_action_policy_allows(&url, is_main, is_blank, should_download) {
        1
    } else {
        0
    }
}

fn navigation_action_is_blank(action: &AnyObject) -> bool {
    let target: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![action, targetFrame] };
    target.is_none()
}

fn navigation_action_is_main_frame(action: &AnyObject) -> bool {
    let target: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![action, targetFrame] };
    target
        .as_deref()
        .is_some_and(|frame| unsafe { objc2::msg_send![frame, isMainFrame] })
}

fn navigation_action_should_download(action: &AnyObject) -> bool {
    let responds: bool =
        unsafe { objc2::msg_send![action, respondsToSelector: sel!(shouldPerformDownload)] };
    if !responds {
        return false;
    }
    unsafe { objc2::msg_send![action, shouldPerformDownload] }
}

fn navigation_action_url(action: &AnyObject) -> Option<String> {
    let request: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![action, request] };
    let request = request?;
    let url: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*request, URL] };
    let url = url?;
    let abs: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*url, absoluteString] };
    nsstring_to_string(abs.as_deref())
}

fn navigation_response_url(response: &AnyObject) -> Option<String> {
    let url_response: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![response, response] };
    let url_response = url_response?;
    let url: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*url_response, URL] };
    let url = url?;
    let abs: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*url, absoluteString] };
    nsstring_to_string(abs.as_deref())
}

fn navigation_response_is_download(nav_response: &AnyObject) -> bool {
    let response: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![nav_response, response] };
    let Some(response) = response else {
        return false;
    };
    let mime: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*response, MIMEType] };
    if let Some(mime) = nsstring_to_string(mime.as_deref()) {
        let mime = mime.to_ascii_lowercase();
        if mime == "application/octet-stream"
            || mime == "application/zip"
            || mime.starts_with("application/x-")
        {
            return true;
        }
    }
    let responds: bool =
        unsafe { objc2::msg_send![&*response, respondsToSelector: sel!(allHeaderFields)] };
    if !responds {
        return false;
    }
    let headers: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![&*response, allHeaderFields] };
    let Some(headers) = headers else {
        return false;
    };
    let Some(key) = nsstring("Content-Disposition") else {
        return false;
    };
    let value: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![&*headers, objectForKey: &*key] };
    nsstring_to_string(value.as_deref())
        .is_some_and(|disp| disp.to_ascii_lowercase().contains("attachment"))
}

fn open_panel_allows_directories(params: *mut AnyObject) -> bool {
    if params.is_null() {
        return false;
    }
    let params = unsafe { &*params };
    let responds: bool =
        unsafe { objc2::msg_send![params, respondsToSelector: sel!(allowsDirectories)] };
    if !responds {
        return false;
    }
    unsafe { objc2::msg_send![params, allowsDirectories] }
}

fn attached_ui_delegate(webview: &AnyObject) -> HarnessResult<Retained<AnyObject>> {
    let delegate: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![webview, UIDelegate] };
    delegate.ok_or_else(|| HarnessError::backend_unavailable("live WK UIDelegate is not attached"))
}

fn probe_open_panel_parameters(allows_directories: bool) -> HarnessResult<Retained<AnyObject>> {
    let cls = open_panel_probe_class(allows_directories).ok_or_else(|| {
        HarnessError::backend_unavailable("open-panel probe parameters class unavailable")
    })?;
    let obj: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, new] };
    obj.ok_or_else(|| HarnessError::backend_unavailable("open-panel probe parameters alloc failed"))
}

fn open_panel_probe_class(allows_directories: bool) -> Option<&'static AnyClass> {
    static YES: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    static NO: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    let slot = if allows_directories { &YES } else { &NO };
    *slot.get_or_init(|| {
        let name = if allows_directories {
            c"GrokptahCbV0OpenPanelParamsDir"
        } else {
            c"GrokptahCbV0OpenPanelParamsFile"
        };
        if let Some(existing) = AnyClass::get(name) {
            return Some(existing);
        }
        let mut builder = ClassBuilder::new(name, NSObject::class())?;
        unsafe {
            if allows_directories {
                builder.add_method(
                    sel!(allowsDirectories),
                    probe_allows_directories_yes as unsafe extern "C-unwind" fn(_, _) -> Bool,
                );
            } else {
                builder.add_method(
                    sel!(allowsDirectories),
                    probe_allows_directories_no as unsafe extern "C-unwind" fn(_, _) -> Bool,
                );
            }
            builder.add_method(
                sel!(allowsMultipleSelection),
                probe_allows_directories_no as unsafe extern "C-unwind" fn(_, _) -> Bool,
            );
        }
        Some(builder.register())
    })
}

unsafe extern "C-unwind" fn probe_allows_directories_yes(_this: &AnyObject, _cmd: Sel) -> Bool {
    Bool::YES
}

unsafe extern "C-unwind" fn probe_allows_directories_no(_this: &AnyObject, _cmd: Sel) -> Bool {
    Bool::NO
}

#[repr(C)]
struct NavigationDecisionBlock {
    isa: *const std::ffi::c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut NavigationDecisionBlock, isize),
}

fn invoke_navigation_decision(handler: *mut std::ffi::c_void, policy: isize) {
    if handler.is_null() {
        return;
    }
    unsafe {
        let block = handler as *mut NavigationDecisionBlock;
        if (*block).invoke as usize != 0 {
            ((*block).invoke)(block, policy);
        }
    }
}

#[repr(C)]
struct ObjectCompletionBlock {
    isa: *const std::ffi::c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut ObjectCompletionBlock, *mut AnyObject),
}

fn invoke_object_completion(handler: *mut std::ffi::c_void, object: *mut AnyObject) {
    if handler.is_null() {
        return;
    }
    unsafe {
        let block = handler as *mut ObjectCompletionBlock;
        if (*block).invoke as usize != 0 {
            ((*block).invoke)(block, object);
        }
    }
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
