//! Smallest honest native WKWebView raster for receipt-gated capture.
//!
//! Creates a real offscreen `WKWebView`, loads a tiny HTML fixture, and copies
//! RGBA8 from `takeSnapshotWithConfiguration:completionHandler:` (WK-composited
//! pixels, not `NSView` backing-store white). This is not isolation PASS and
//! never enables admission. Uniform window-white fail-closes.

use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
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
    admit_native_capability, admit_navigation, is_download_probe_url,
    live_wk_navigation_action_policy_allows, live_wk_navigation_response_policy_allows,
    live_wk_open_panel_policy_allows, owned_page_for_boot, NativeDenyKind, DOWNLOAD_PROBE_FILENAME,
    DOWNLOAD_PROBE_SCHEME,
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
    "<a id=\"cb-v0-download\" href=\"grokptah-cbv0://owned/deny.bin\" download=\"deny.bin\">dl</a>",
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
    "if(!el){return 'missing';}",
    "el.href='grokptah-cbv0://owned/deny.bin';",
    "el.setAttribute('download','deny.bin');",
    "el.click();return el.href||'clicked';})()"
);

const BLANK_CLICK_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-blank');",
    "if(!el){return 'missing';}el.click();return 'clicked';})()"
);

const FILE_ARM_JS: &str = concat!(
    "(function(){var wrap=document.getElementById('cb-v0-probes');",
    "var el=document.getElementById('cb-v0-file');",
    "var other=document.getElementById('cb-v0-dir');",
    "if(!el){return 'missing';}",
    "if(wrap){wrap.style.cssText='display:block;position:fixed;inset:0;z-index:9999;';}",
    "if(other){other.style.display='none';}",
    "el.style.cssText='position:fixed;left:0;top:0;width:16px;height:16px;opacity:0.2;margin:0;padding:0;border:0;display:block;';",
    "window.__cbV0OpenPanel='armed';",
    "document.addEventListener('click',function(){",
    "try{if(el.showPicker){el.showPicker();window.__cbV0OpenPanel='showPicker';}",
    "else{el.click();window.__cbV0OpenPanel='click';}}",
    "catch(e){window.__cbV0OpenPanel='err:'+(e&&e.name||'x');}",
    "},{once:true,capture:true});",
    "return 'armed';})()"
);

const DIR_ARM_JS: &str = concat!(
    "(function(){var wrap=document.getElementById('cb-v0-probes');",
    "var el=document.getElementById('cb-v0-dir');",
    "var other=document.getElementById('cb-v0-file');",
    "if(!el){return 'missing';}",
    "if(wrap){wrap.style.cssText='display:block;position:fixed;inset:0;z-index:9999;';}",
    "if(other){other.style.display='none';}",
    "el.style.cssText='position:fixed;left:0;top:0;width:16px;height:16px;opacity:0.2;margin:0;padding:0;border:0;display:block;';",
    "window.__cbV0OpenPanel='armed';",
    "document.addEventListener('click',function(){",
    "try{if(el.showPicker){el.showPicker();window.__cbV0OpenPanel='showPicker';}",
    "else{el.click();window.__cbV0OpenPanel='click';}}",
    "catch(e){window.__cbV0OpenPanel='err:'+(e&&e.name||'x');}",
    "},{once:true,capture:true});",
    "return 'armed';})()"
);

const FILE_ACTIVATE_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-file');",
    "if(!el){return 'missing';}",
    "try{if(el.showPicker){el.showPicker();return 'showPicker';}}catch(e){}",
    "el.click();return 'clicked';})()"
);

const DIR_ACTIVATE_JS: &str = concat!(
    "(function(){var el=document.getElementById('cb-v0-dir');",
    "if(!el){return 'missing';}",
    "try{if(el.showPicker){el.showPicker();return 'showPicker';}}catch(e){}",
    "el.click();return 'clicked';})()"
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
    window: Option<Retained<AnyObject>>,
    _delegate: Retained<AnyObject>,
    _scheme_handler: Retained<AnyObject>,
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
/// `WKNavigationActionPolicyDownload` / `WKNavigationResponsePolicyDownload`.
const WK_NAVIGATION_POLICY_DOWNLOAD: isize = 2;

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
        let scheme_handler = attach_download_scheme_handler(&config)?;

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
            window,
            _delegate: delegate,
            _scheme_handler: scheme_handler,
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

    /// Last `runOpenPanel` completion: (allows_directories, urls_null, wk_originated).
    /// `wk_originated` is true only when WK delivered `WKOpenPanelParameters`
    /// (not a probe NSObject poke of the attached UIDelegate IMP).
    pub(crate) fn last_open_panel_deny(&self) -> Option<(bool, bool, bool)> {
        last_recorded_open_panel(self.webview_key()).map(|record| {
            (
                record.allows_directories,
                record.urls_null,
                record.wk_originated,
            )
        })
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

    /// Click dedicated `grokptah-cbv0://owned/deny.bin`. Action policy Allows
    /// that fetch (not a page admit) so `WKURLSchemeHandler` can serve
    /// `Content-Disposition: attachment` + octet-stream. Response policy
    /// returns `WKNavigationResponsePolicyDownload` (2). Deny is a real
    /// non-null `didBecomeDownload` plus `decideDestination` completing nil
    /// after inspecting the attachment. Dummy IMP pokes, blob cancels, and
    /// `shouldPerformDownload` shortcuts are not a deny. No file is written.
    pub(crate) fn attempt_download(&self) -> HarnessResult<()> {
        require_main_thread("live WK download deny")?;
        let key = self.webview_key();
        clear_native_denies(key);
        clear_navigation_decisions(key);
        clear_download_proof();
        let result = evaluate_javascript_value(&self.webview, DOWNLOAD_CLICK_JS)?;
        if result.as_deref() == Some("missing") {
            return Err(HarnessError::backend_unavailable(
                "owned-page download probe anchor is missing",
            ));
        }
        wait_for_native_deny(key, NativeDenyKind::Download).map_err(|err| {
            HarnessError::backend_unavailable(format!(
                "WK did not originate a download deny via WKDownload: {err}"
            ))
        })?;
        wait_for_wk_download_proof()?;
        let promoted = wait_for_download_navigation_policy(key, WK_NAVIGATION_POLICY_DOWNLOAD)?;
        if !is_download_probe_url(&promoted.0) {
            return Err(HarnessError::invalid_state(format!(
                "WK Download policy 2 was not the dedicated scheme URL, got {}",
                promoted.0
            )));
        }
        let decision = wait_for_download_navigation_cancel(key)?;
        if decision.1 != 0 {
            return Err(HarnessError::invalid_state(format!(
                "WK download must end cancelled after WKDownload (url={} policy={})",
                decision.0, decision.1
            )));
        }
        if !is_download_probe_url(&decision.0) {
            return Err(HarnessError::invalid_state(format!(
                "WK download cancel was not the dedicated scheme URL, got {}",
                decision.0
            )));
        }
        assert_download_file_not_written()?;
        self.assert_still_owned("download")
    }

    /// File picker: owned-page `<input type=file>` must make WK consult
    /// `WKUIDelegate.runOpenPanelWithParameters` with a real
    /// `WKOpenPanelParameters`. The IMP fail-closes with nil URLs.
    /// Timeout → self `objc_msgSend` of `runOpenPanel` is not a deny.
    pub(crate) fn attempt_file_picker(&self) -> HarnessResult<()> {
        self.attempt_open_panel_from_owned_page(false)
    }

    /// Directory picker: owned-page `<input webkitdirectory>` must make WK
    /// consult `runOpenPanel` with `allowsDirectories`. Same nil-URL deny.
    pub(crate) fn attempt_directory_picker(&self) -> HarnessResult<()> {
        self.attempt_open_panel_from_owned_page(true)
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
        require_create_webview_imp(&self.webview)?;
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

    fn attempt_open_panel_from_owned_page(&self, allows_directories: bool) -> HarnessResult<()> {
        require_main_thread("live WK open-panel deny")?;
        let kind = if allows_directories {
            NativeDenyKind::DirectoryPicker
        } else {
            NativeDenyKind::FilePicker
        };
        let key = self.webview_key();
        clear_native_denies(key);
        clear_open_panel_completions(key);
        require_ui_delegate_would_receive_open_panel(&self.webview)?;
        let armed = evaluate_javascript_value(
            &self.webview,
            if allows_directories {
                DIR_ARM_JS
            } else {
                FILE_ARM_JS
            },
        )?;
        if armed.as_deref() == Some("missing") {
            return Err(HarnessError::backend_unavailable(format!(
                "owned-page {} probe input is missing",
                kind.as_str()
            )));
        }
        let _: () = unsafe { objc2::msg_send![&*self.webview, layoutSubtreeIfNeeded] };
        for _ in 0..4 {
            pump_runloop_briefly();
        }
        deliver_webview_mouse_click(self.window.as_deref(), &self.webview)?;
        let activated = evaluate_javascript_value(
            &self.webview,
            if allows_directories {
                DIR_ACTIVATE_JS
            } else {
                FILE_ACTIVATE_JS
            },
        )?;
        if activated.as_deref() == Some("missing") {
            return Err(HarnessError::backend_unavailable(format!(
                "owned-page {} probe input disappeared",
                kind.as_str()
            )));
        }
        wait_for_native_deny(key, kind).map_err(|err| {
            HarnessError::backend_unavailable(format!(
                "WK did not consult runOpenPanel for {} (JS/gesture click without UIDelegate is not a deny): {err}",
                kind.as_str()
            ))
        })?;
        wait_for_wk_open_panel_deny(key, allows_directories)?;
        if let Some(window) = self.window.as_deref() {
            let _: () = unsafe { objc2::msg_send![window, orderBack: None::<&AnyObject>] };
        }
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

fn attach_download_scheme_handler(config: &AnyObject) -> HarnessResult<Retained<AnyObject>> {
    let responds: bool = unsafe {
        objc2::msg_send![
            config,
            respondsToSelector: sel!(setURLSchemeHandler:forURLScheme:)
        ]
    };
    if !responds {
        return Err(HarnessError::backend_unavailable(
            "WKWebViewConfiguration setURLSchemeHandler:forURLScheme: unavailable",
        ));
    }
    let handler = download_scheme_handler_instance()?;
    let scheme = nsstring(DOWNLOAD_PROBE_SCHEME).ok_or_else(|| {
        HarnessError::backend_unavailable("download probe scheme NSString unavailable")
    })?;
    let _: () =
        unsafe { objc2::msg_send![config, setURLSchemeHandler: &*handler, forURLScheme: &*scheme] };
    Ok(handler)
}

fn download_scheme_handler_class() -> Option<&'static AnyClass> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    *CLASS.get_or_init(|| {
        if let Some(existing) = AnyClass::get(c"GrokptahCbV0DownloadSchemeHandler") {
            return Some(existing);
        }
        let mut builder =
            ClassBuilder::new(c"GrokptahCbV0DownloadSchemeHandler", NSObject::class())?;
        if let Some(protocol) = AnyProtocol::get(c"WKURLSchemeHandler") {
            let _ = builder.add_protocol(protocol);
        }
        unsafe {
            builder.add_method(
                sel!(webView:startURLSchemeTask:),
                start_download_scheme_task as unsafe extern "C-unwind" fn(_, _, _, _),
            );
            builder.add_method(
                sel!(webView:stopURLSchemeTask:),
                stop_download_scheme_task as unsafe extern "C-unwind" fn(_, _, _, _),
            );
        }
        Some(builder.register())
    })
}

fn download_scheme_handler_instance() -> HarnessResult<Retained<AnyObject>> {
    let cls = download_scheme_handler_class().ok_or_else(|| {
        HarnessError::backend_unavailable("download WKURLSchemeHandler class unavailable")
    })?;
    let obj: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, new] };
    obj.ok_or_else(|| HarnessError::backend_unavailable("download WKURLSchemeHandler alloc failed"))
}

unsafe extern "C-unwind" fn start_download_scheme_task(
    _this: &AnyObject,
    _cmd: Sel,
    _webview: *mut AnyObject,
    task: *mut AnyObject,
) {
    if task.is_null() {
        return;
    }
    record_download_proof(|proof| proof.scheme_handler_started = true);
    let task = unsafe { &*task };
    let request: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![task, request] };
    let url: Option<Retained<AnyObject>> = request
        .as_deref()
        .and_then(|request| unsafe { objc2::msg_send![request, URL] });
    let Some(url) = url else {
        return;
    };
    let body = b"DENY";
    let Some(response) = download_probe_url_response(&url, body.len() as isize) else {
        return;
    };
    let Some(data) = nsdata(body) else {
        return;
    };
    record_download_proof(|proof| proof.content_disposition_served = true);
    let _: () = unsafe { objc2::msg_send![task, didReceiveResponse: &*response] };
    let _: () = unsafe { objc2::msg_send![task, didReceiveData: &*data] };
    let _: () = unsafe { objc2::msg_send![task, didFinish] };
}

unsafe extern "C-unwind" fn stop_download_scheme_task(
    _this: &AnyObject,
    _cmd: Sel,
    _webview: *mut AnyObject,
    _task: *mut AnyObject,
) {
}

fn download_probe_url_response(url: &AnyObject, length: isize) -> Option<Retained<AnyObject>> {
    let headers = nsmutable_dictionary()?;
    let content_type_key = nsstring("Content-Type")?;
    let content_type = nsstring("application/octet-stream")?;
    let _: () = unsafe {
        objc2::msg_send![&*headers, setObject: &*content_type, forKey: &*content_type_key]
    };
    let disposition_key = nsstring("Content-Disposition")?;
    let disposition = nsstring("attachment; filename=\"deny.bin\"")?;
    let _: () =
        unsafe { objc2::msg_send![&*headers, setObject: &*disposition, forKey: &*disposition_key] };
    if let Some(http_cls) = AnyClass::get(c"NSHTTPURLResponse") {
        let alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![http_cls, alloc] };
        let version = nsstring("HTTP/1.1")?;
        let response: Option<Retained<AnyObject>> = unsafe {
            objc2::msg_send![
                alloc,
                initWithURL: url,
                statusCode: 200isize,
                HTTPVersion: &*version,
                headerFields: &*headers
            ]
        };
        if response.is_some() {
            return response;
        }
    }
    let resp_cls = AnyClass::get(c"NSURLResponse")?;
    let alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![resp_cls, alloc] };
    let mime = nsstring("application/octet-stream")?;
    unsafe {
        objc2::msg_send![
            alloc,
            initWithURL: url,
            MIMEType: &*mime,
            expectedContentLength: length,
            textEncodingName: None::<&AnyObject>
        ]
    }
}

fn nsmutable_dictionary() -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"NSMutableDictionary")?;
    unsafe { objc2::msg_send![cls, new] }
}

fn nsdata(bytes: &[u8]) -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"NSData")?;
    unsafe { objc2::msg_send![cls, dataWithBytes: bytes.as_ptr(), length: bytes.len()] }
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

fn wait_for_download_navigation_policy(
    key: usize,
    want_policy: isize,
) -> HarnessResult<(String, isize)> {
    let started = Instant::now();
    loop {
        if let Ok(guard) = navigation_decision_log().lock() {
            if let Some(list) = guard.get(&key) {
                if let Some(hit) = list
                    .iter()
                    .rev()
                    .find(|(url, policy)| *policy == want_policy && is_download_probe_url(url))
                {
                    return Ok(hit.clone());
                }
            }
        }
        if started.elapsed() > LOAD_TIMEOUT {
            return Err(HarnessError::backend_unavailable(format!(
                "WK did not return navigation policy {want_policy} for the dedicated grokptah-cbv0 download URL"
            )));
        }
        pump_runloop_briefly();
    }
}

fn wait_for_download_navigation_cancel(key: usize) -> HarnessResult<(String, isize)> {
    wait_for_download_navigation_policy(key, 0)
}

#[derive(Debug, Default, Clone)]
struct DownloadProof {
    scheme_handler_started: bool,
    content_disposition_served: bool,
    become_download_nonnull: bool,
    destination_invoked: bool,
    destination_nil: bool,
    destination_attachment: bool,
}

fn download_proof() -> &'static Mutex<DownloadProof> {
    static LOG: OnceLock<Mutex<DownloadProof>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(DownloadProof::default()))
}

fn record_download_proof(update: impl FnOnce(&mut DownloadProof)) {
    if let Ok(mut guard) = download_proof().lock() {
        update(&mut guard);
    }
}

fn clear_download_proof() {
    if let Ok(mut guard) = download_proof().lock() {
        *guard = DownloadProof::default();
    }
}

fn snapshot_download_proof() -> DownloadProof {
    download_proof()
        .lock()
        .ok()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

fn wait_for_wk_download_proof() -> HarnessResult<()> {
    let started = Instant::now();
    loop {
        let proof = snapshot_download_proof();
        if proof.become_download_nonnull
            && proof.scheme_handler_started
            && proof.content_disposition_served
            && proof.destination_invoked
            && proof.destination_nil
            && proof.destination_attachment
        {
            return Ok(());
        }
        if started.elapsed() > LOAD_TIMEOUT {
            return Err(HarnessError::backend_unavailable(format!(
                "WKDownload proof incomplete: scheme_handler={} disposition={} become_download={} destination_invoked={} destination_nil={} attachment={}",
                proof.scheme_handler_started,
                proof.content_disposition_served,
                proof.become_download_nonnull,
                proof.destination_invoked,
                proof.destination_nil,
                proof.destination_attachment
            )));
        }
        pump_runloop_briefly();
    }
}

fn refuse_and_record(key: usize, kind: NativeDenyKind) {
    // Same fail-closed gate Linux `refuse_native_capability` tests call.
    if admit_native_capability(kind).is_err() {
        record_native_deny(key, kind);
    }
}

fn require_create_webview_imp(webview: &AnyObject) -> HarnessResult<()> {
    let delegate = attached_ui_delegate(webview)?;
    let responds: bool = unsafe {
        objc2::msg_send![
            &*delegate,
            respondsToSelector: sel!(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:)
        ]
    };
    if !responds {
        return Err(HarnessError::backend_unavailable(
            "UIDelegate createWebView IMP is missing; unimplemented createWebView is fail-open so the probe fail-closes",
        ));
    }
    Ok(())
}

fn assert_download_file_not_written() -> HarnessResult<()> {
    let proof = snapshot_download_proof();
    if !proof.destination_invoked {
        return Err(HarnessError::invalid_state(
            "WKDownload decideDestination was not invoked; file-not-written is not proved",
        ));
    }
    if !proof.destination_nil {
        return Err(HarnessError::invalid_state(
            "WKDownload decideDestination received a file URL; download must complete with nil destination",
        ));
    }
    for path in download_probe_file_candidates() {
        if path.is_file() {
            return Err(HarnessError::invalid_state(format!(
                "download probe wrote {}; v0 must not save a file",
                path.display()
            )));
        }
    }
    Ok(())
}

fn download_probe_file_candidates() -> Vec<PathBuf> {
    let mut paths = vec![std::env::temp_dir().join(DOWNLOAD_PROBE_FILENAME)];
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(DOWNLOAD_PROBE_FILENAME));
    }
    if let Ok(home) = std::env::var("HOME") {
        paths.push(
            Path::new(&home)
                .join("Downloads")
                .join(DOWNLOAD_PROBE_FILENAME),
        );
    }
    if let Some(tmp) = nstemporary_directory() {
        paths.push(tmp.join(DOWNLOAD_PROBE_FILENAME));
    }
    paths
}

fn nstemporary_directory() -> Option<PathBuf> {
    ns_temporary_directory_path()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

fn ns_temporary_directory_path() -> Option<String> {
    let cls = AnyClass::get(c"NSFileManager")?;
    let fm: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, defaultManager] };
    let fm = fm?;
    let dir: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*fm, temporaryDirectory] };
    let dir = dir?;
    let path: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*dir, path] };
    nsstring_to_string(path.as_deref())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenPanelDenyRecord {
    allows_directories: bool,
    urls_null: bool,
    wk_originated: bool,
}

type OpenPanelCompletionLog = HashMap<usize, Vec<OpenPanelDenyRecord>>;

fn open_panel_completion_log() -> &'static Mutex<OpenPanelCompletionLog> {
    static LOG: OnceLock<Mutex<OpenPanelCompletionLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_open_panel_completion(key: usize, record: OpenPanelDenyRecord) {
    if let Ok(mut guard) = open_panel_completion_log().lock() {
        guard.entry(key).or_default().push(record);
    }
}

fn clear_open_panel_completions(key: usize) {
    if let Ok(mut guard) = open_panel_completion_log().lock() {
        guard.insert(key, Vec::new());
    }
}

fn last_recorded_open_panel(key: usize) -> Option<OpenPanelDenyRecord> {
    open_panel_completion_log()
        .lock()
        .ok()?
        .get(&key)?
        .last()
        .copied()
}

fn wait_for_wk_open_panel_deny(
    key: usize,
    allows_directories: bool,
) -> HarnessResult<OpenPanelDenyRecord> {
    let started = Instant::now();
    loop {
        if let Ok(guard) = open_panel_completion_log().lock() {
            if let Some(list) = guard.get(&key) {
                if let Some(hit) = list
                    .iter()
                    .rev()
                    .find(|record| record.allows_directories == allows_directories)
                {
                    if !hit.urls_null {
                        return Err(HarnessError::invalid_state(
                            "runOpenPanel completionHandler received URLs; picker must deny with nil",
                        ));
                    }
                    if !hit.wk_originated {
                        return Err(HarnessError::backend_unavailable(
                            "runOpenPanel was not WK-originated (WKOpenPanelParameters required; IMP poke is not a deny)",
                        ));
                    }
                    return Ok(*hit);
                }
                if list.iter().any(|record| !record.urls_null) {
                    return Err(HarnessError::invalid_state(
                        "runOpenPanel completionHandler received URLs; picker must deny with nil",
                    ));
                }
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
    let key = webview as usize;
    // Dedicated scheme URL: Allow the fetch so WKURLSchemeHandler can serve
    // the attachment. Not a page admit. Response returns policy 2.
    // shouldPerformDownload on any other URL cancels without stamping Download
    // (that flag is not a WKDownload instance).
    if is_download_probe_url(&url) {
        record_navigation_decision(key, url, 1);
        invoke_navigation_decision(decision_handler, 1);
        return;
    }
    if should_download {
        record_navigation_decision(key, url, 0);
        invoke_navigation_decision(decision_handler, 0);
        return;
    }

    let policy = navigation_action_policy(action);
    if policy == 0 && is_blank {
        refuse_and_record(key, NativeDenyKind::NewWindow);
    }
    record_navigation_decision(key, url, policy);
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
    // Dedicated attachment resource: promote to a real WKDownload. Never
    // Allow-as-page, never stamp Download here. didBecomeDownload is the oracle.
    if is_download_probe_url(&url) {
        record_navigation_decision(webview as usize, url, WK_NAVIGATION_POLICY_DOWNLOAD);
        invoke_navigation_decision(decision_handler, WK_NAVIGATION_POLICY_DOWNLOAD);
        return;
    }
    let allow = live_wk_navigation_response_policy_allows(&url, is_main, can_show, is_download);
    let policy = if allow { 1 } else { 0 };
    invoke_navigation_decision(decision_handler, policy);
}

unsafe extern "C-unwind" fn navigation_action_became_download(
    this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    action: *mut AnyObject,
    download: *mut AnyObject,
) {
    if download.is_null() {
        return;
    }
    let url = if action.is_null() {
        wk_download_url(download)
    } else {
        navigation_action_url(unsafe { &*action }).or_else(|| wk_download_url(download))
    };
    let Some(url) = url else {
        cancel_wk_download(this, download);
        return;
    };
    if !is_download_probe_url(&url) {
        cancel_wk_download(this, download);
        return;
    }
    record_download_proof(|proof| proof.become_download_nonnull = true);
    let _: () = unsafe { objc2::msg_send![&*download, setDelegate: this] };
    refuse_and_record(webview as usize, NativeDenyKind::Download);
    record_navigation_decision(webview as usize, url, 0);
}

unsafe extern "C-unwind" fn navigation_response_became_download(
    this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    response: *mut AnyObject,
    download: *mut AnyObject,
) {
    if download.is_null() {
        return;
    }
    let url = if response.is_null() {
        wk_download_url(download)
    } else {
        navigation_response_url(unsafe { &*response }).or_else(|| wk_download_url(download))
    };
    let Some(url) = url else {
        cancel_wk_download(this, download);
        return;
    };
    if !is_download_probe_url(&url) {
        cancel_wk_download(this, download);
        return;
    }
    record_download_proof(|proof| proof.become_download_nonnull = true);
    let _: () = unsafe { objc2::msg_send![&*download, setDelegate: this] };
    refuse_and_record(webview as usize, NativeDenyKind::Download);
    record_navigation_decision(webview as usize, url, 0);
}

unsafe extern "C-unwind" fn download_decide_destination(
    _this: &AnyObject,
    _cmd: Sel,
    _download: *mut AnyObject,
    response: *mut AnyObject,
    _filename: *mut AnyObject,
    decision_handler: *mut std::ffi::c_void,
) {
    let _ = admit_native_capability(NativeDenyKind::Download);
    let attachment = url_response_is_attachment_or_octet_stream(response);
    record_download_proof(|proof| {
        proof.destination_invoked = true;
        proof.destination_nil = true;
        proof.destination_attachment = attachment;
    });
    invoke_object_completion(decision_handler, std::ptr::null_mut());
}

fn url_response_is_attachment_or_octet_stream(response: *mut AnyObject) -> bool {
    if response.is_null() {
        return false;
    }
    let response = unsafe { &*response };
    let mime: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![response, MIMEType] };
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
        unsafe { objc2::msg_send![response, respondsToSelector: sel!(allHeaderFields)] };
    if !responds {
        return false;
    }
    let headers: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![response, allHeaderFields] };
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

unsafe extern "C-unwind" fn create_webview(
    _this: &AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    _config: *mut AnyObject,
    _action: *mut AnyObject,
    features: *mut AnyObject,
) -> *mut AnyObject {
    let kind = create_webview_deny_kind(features);
    refuse_and_record(webview as usize, kind);
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
    frame: *mut AnyObject,
    decision_handler: *mut std::ffi::c_void,
) {
    let allows_directories = open_panel_allows_directories(params);
    let kind = if allows_directories {
        NativeDenyKind::DirectoryPicker
    } else {
        NativeDenyKind::FilePicker
    };
    let wk_originated = open_panel_is_wk_originated(params, frame);
    if !live_wk_open_panel_policy_allows(allows_directories) {
        record_open_panel_completion(
            webview as usize,
            OpenPanelDenyRecord {
                allows_directories,
                urls_null: true,
                wk_originated,
            },
        );
        // Only a WK-delivered WKOpenPanelParameters call records FilePicker /
        // DirectoryPicker. Probe NSObject / WKWebView-as-frame pokes fail closed
        // (nil URLs) without counting as a deny.
        if wk_originated {
            record_native_deny(webview as usize, kind);
        }
    }
    invoke_object_completion(decision_handler, std::ptr::null_mut());
}

fn wk_download_url(download: *mut AnyObject) -> Option<String> {
    if download.is_null() {
        return None;
    }
    let download = unsafe { &*download };
    let responds: bool =
        unsafe { objc2::msg_send![download, respondsToSelector: sel!(originalRequest)] };
    if !responds {
        return None;
    }
    let request: Option<Retained<AnyObject>> =
        unsafe { objc2::msg_send![download, originalRequest] };
    let request = request?;
    let url: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*request, URL] };
    let url = url?;
    let abs: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![&*url, absoluteString] };
    nsstring_to_string(abs.as_deref())
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

/// WK's chrome client calls `runOpenPanel` only when the UIDelegate responds
/// to the selector. Missing IMP would also complete with nil URLs, so this
/// proves our deny IMP is the one WK would deliver to.
fn require_ui_delegate_would_receive_open_panel(webview: &AnyObject) -> HarnessResult<()> {
    let delegate = attached_ui_delegate(webview)?;
    let responds: bool = unsafe {
        objc2::msg_send![
            &*delegate,
            respondsToSelector: sel!(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:)
        ]
    };
    if !responds {
        return Err(HarnessError::backend_unavailable(
            "attached UIDelegate does not implement runOpenPanel; WK would not deliver the file chooser to our deny IMP",
        ));
    }
    Ok(())
}

fn open_panel_is_wk_originated(params: *mut AnyObject, frame: *mut AnyObject) -> bool {
    if !params_are_wk_open_panel_parameters(params) {
        return false;
    }
    if frame.is_null() {
        return true;
    }
    let frame = unsafe { &*frame };
    if let Some(webview_cls) = AnyClass::get(c"WKWebView") {
        let is_webview: bool = unsafe { objc2::msg_send![frame, isKindOfClass: webview_cls] };
        if is_webview {
            // #576 poke passed the WKWebView as initiatedByFrame.
            return false;
        }
    }
    true
}

fn params_are_wk_open_panel_parameters(params: *mut AnyObject) -> bool {
    if params.is_null() {
        return false;
    }
    let Some(cls) = AnyClass::get(c"WKOpenPanelParameters") else {
        return false;
    };
    let params = unsafe { &*params };
    unsafe { objc2::msg_send![params, isKindOfClass: cls] }
}

const NS_EVENT_LEFT_MOUSE_DOWN: usize = 1;
const NS_EVENT_LEFT_MOUSE_UP: usize = 2;

fn deliver_webview_mouse_click(
    window: Option<&AnyObject>,
    webview: &AnyObject,
) -> HarnessResult<()> {
    let location = CGPoint {
        x: LOGICAL_WIDTH / 2.0,
        y: LOGICAL_HEIGHT / 2.0,
    };
    let timestamp = process_uptime();
    let window_number: isize = if let Some(window) = window {
        let _: () = unsafe { objc2::msg_send![window, orderFrontRegardless] };
        let _: () = unsafe { objc2::msg_send![window, makeKeyAndOrderFront: None::<&AnyObject>] };
        let _: bool = unsafe { objc2::msg_send![window, makeFirstResponder: webview] };
        unsafe { objc2::msg_send![window, windowNumber] }
    } else {
        0
    };
    post_mouse_event(
        window,
        webview,
        NS_EVENT_LEFT_MOUSE_DOWN,
        location,
        window_number,
        timestamp,
    )?;
    pump_runloop_briefly();
    post_mouse_event(
        window,
        webview,
        NS_EVENT_LEFT_MOUSE_UP,
        location,
        window_number,
        timestamp,
    )?;
    for _ in 0..4 {
        pump_runloop_briefly();
    }
    Ok(())
}

fn post_mouse_event(
    window: Option<&AnyObject>,
    webview: &AnyObject,
    event_type: usize,
    location: CGPoint,
    window_number: isize,
    timestamp: f64,
) -> HarnessResult<()> {
    let event_cls = AnyClass::get(c"NSEvent").ok_or_else(|| {
        HarnessError::backend_unavailable("NSEvent unavailable for open-panel gesture")
    })?;
    let event: Option<Retained<AnyObject>> = unsafe {
        objc2::msg_send![
            event_cls,
            mouseEventWithType: event_type,
            location: location,
            modifierFlags: 0usize,
            timestamp: timestamp,
            windowNumber: window_number,
            context: None::<&AnyObject>,
            eventNumber: 0isize,
            clickCount: 1isize,
            pressure: 1.0f32
        ]
    };
    let event = event.ok_or_else(|| {
        HarnessError::backend_unavailable("NSEvent mouse event alloc failed for open-panel gesture")
    })?;
    if let Some(window) = window {
        let _: () = unsafe { objc2::msg_send![window, sendEvent: &*event] };
    }
    match event_type {
        NS_EVENT_LEFT_MOUSE_DOWN => {
            let _: () = unsafe { objc2::msg_send![webview, mouseDown: &*event] };
        }
        NS_EVENT_LEFT_MOUSE_UP => {
            let _: () = unsafe { objc2::msg_send![webview, mouseUp: &*event] };
        }
        _ => {}
    }
    Ok(())
}

fn process_uptime() -> f64 {
    let Some(cls) = AnyClass::get(c"NSProcessInfo") else {
        return 0.0;
    };
    let info: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, processInfo] };
    let Some(info) = info else {
        return 0.0;
    };
    unsafe { objc2::msg_send![&*info, systemUptime] }
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
