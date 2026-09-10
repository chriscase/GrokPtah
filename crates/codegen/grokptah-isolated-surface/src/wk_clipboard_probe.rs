//! WKClipboardProbe — private [`WKContentWorld`] pull/reply protocol.
//!
//! The contained **page world** must never be targeted with
//! `evaluateJavaScript` or `callAsyncJavaScript`. Host pull happens only in a
//! private probe world. Replies are typed, generation-bound, and fail closed on
//! timeout, navigation/epoch drift, malformation, or duplicates.

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, HarnessResult};

/// Private content-world name. Must not collide with the page world.
pub const PRIVATE_PROBE_WORLD_NAME: &str = "grokptah.clipboard.probe.v1";

/// Script-message name used only in the private world.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PRIVATE_REPLY_HANDLER_NAME: &str = "grokptahClipboardReply";

/// Maximum wait for a private-world reply.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_000);

/// Clipboard operations the kill-gate must mediate page-locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardOperation {
    Copy,
    Cut,
    Paste,
    Write,
}

impl ClipboardOperation {
    pub const ALL: [ClipboardOperation; 4] = [
        ClipboardOperation::Copy,
        ClipboardOperation::Cut,
        ClipboardOperation::Paste,
        ClipboardOperation::Write,
    ];
}

/// Which JS world produced or observed a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentWorld {
    /// Untrusted contained page. Host must never evaluate into this world.
    Page,
    /// Host-only probe world.
    PrivateProbe,
}

/// How the host obtained the reply. Page-world evaluation is forbidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptEvaluationPath {
    PrivateContentWorldPull,
    PageWorldEvaluateJavaScript,
    CallAsyncJavaScript,
}

/// Fail-closed reasons. None of these may seal a Pass verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardProbeFailClosedReason {
    Timeout,
    NavigationEpochDrift,
    MalformedReply,
    DuplicateReply,
    MissingReply,
    StaleGeneration,
    UnexpectedHostClipboardChange,
    UnsupportedPlatform,
    MissingWebKitCapability,
    PermissionPrompt,
    Uncertain,
    ForbiddenScriptEvaluation,
}

/// Host pull request bound to one generation and navigation epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbePull {
    pub generation: u64,
    pub epoch: u64,
    pub world: ContentWorld,
    pub path: ScriptEvaluationPath,
}

impl ProbePull {
    pub fn private(generation: u64, epoch: u64) -> Self {
        Self {
            generation,
            world: ContentWorld::PrivateProbe,
            path: ScriptEvaluationPath::PrivateContentWorldPull,
            epoch,
        }
    }
}

/// One page-local mediation receipt. Never carries clipboard payload bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageLocalClipboardReceipt {
    pub operation: ClipboardOperation,
    pub generation: u64,
    pub epoch: u64,
    pub mediated_page_local: bool,
    pub host_pasteboard_touched: bool,
}

/// Private-world reply to a pull.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReply {
    pub generation: u64,
    pub epoch: u64,
    pub world: ContentWorld,
    pub path: ScriptEvaluationPath,
    pub receipts: Vec<PageLocalClipboardReceipt>,
}

/// Admitted reply after protocol checks. Never a Pass claim by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmittedProbeReply {
    pub generation: u64,
    pub epoch: u64,
    pub receipts: Vec<PageLocalClipboardReceipt>,
}

/// Page-world user script: intercept copy/cut/paste/write into a page-local
/// store. Never invokes host pasteboard APIs. Installed as a WKUserScript at
/// document-start in the **page** world (not via evaluateJavaScript).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PAGE_WORLD_INTERCEPTOR_SOURCE: &str = r#"
(function () {
  if (window.__grokptahClipboardInterceptorInstalled) { return; }
  window.__grokptahClipboardInterceptorInstalled = true;
  var store = "";
  function record(op) {
    var root = document.documentElement;
    var raw = root.getAttribute("data-grokptah-clipboard-receipts") || "[]";
    var list = [];
    try { list = JSON.parse(raw); } catch (e) { list = []; }
    list.push({
      operation: op,
      mediatedPageLocal: true,
      hostPasteboardTouched: false
    });
    root.setAttribute("data-grokptah-clipboard-receipts", JSON.stringify(list));
  }
  document.addEventListener("copy", function (e) {
    e.preventDefault();
    store = (window.getSelection && String(window.getSelection())) || store;
    record("copy");
  }, true);
  document.addEventListener("cut", function (e) {
    e.preventDefault();
    store = (window.getSelection && String(window.getSelection())) || store;
    record("cut");
  }, true);
  document.addEventListener("paste", function (e) {
    e.preventDefault();
    record("paste");
  }, true);
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText = function (text) {
      store = String(text || "");
      record("write");
      return Promise.resolve();
    };
  }
  if (navigator.clipboard && navigator.clipboard.write) {
    navigator.clipboard.write = function () {
      record("write");
      return Promise.resolve();
    };
  }
})();
"#;

/// Private-world pull script. Host evaluates **only** this world.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PRIVATE_WORLD_PULL_SOURCE: &str = r#"
(function () {
  window.grokptahClipboardPull = function (generation, epoch) {
    var root = document.documentElement;
    var raw = root.getAttribute("data-grokptah-clipboard-receipts") || "[]";
    var list = [];
    try { list = JSON.parse(raw); } catch (e) { list = []; }
    var field = document.getElementById("probe-field");
    if (field) {
      field.focus();
      field.select();
    }
    try { document.execCommand("copy"); } catch (e) {}
    try { document.execCommand("cut"); } catch (e) {}
    try { document.execCommand("paste"); } catch (e) {}
    if (navigator.clipboard && navigator.clipboard.writeText) {
      try { navigator.clipboard.writeText("page-local-write"); } catch (e) {}
    }
    raw = root.getAttribute("data-grokptah-clipboard-receipts") || "[]";
    try { list = JSON.parse(raw); } catch (e) { list = []; }
    var receipts = list.map(function (item) {
      return {
        operation: item.operation,
        generation: generation,
        epoch: epoch,
        mediatedPageLocal: !!item.mediatedPageLocal,
        hostPasteboardTouched: !!item.hostPasteboardTouched
      };
    });
    var reply = {
      generation: generation,
      epoch: epoch,
      world: "private_probe",
      path: "private_content_world_pull",
      receipts: receipts
    };
    root.setAttribute("data-grokptah-probe-reply", JSON.stringify(reply));
    document.title = "GROKPTAH-CLIPBOARD-REPLY:" + JSON.stringify(reply);
    if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.grokptahClipboardReply) {
      window.webkit.messageHandlers.grokptahClipboardReply.postMessage(reply);
    }
    return JSON.stringify(reply);
  };
})();
"#;

/// Prefix written to `document.title` so the host can poll without evaluating
/// into the page world.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PRIVATE_REPLY_TITLE_PREFIX: &str = "GROKPTAH-CLIPBOARD-REPLY:";

/// Disposable fixture HTML. No network, no auth, no private data.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PROBE_FIXTURE_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>GrokPtah clipboard kill-gate</title>
<input id="probe-field" value="page-local-probe-text">
"#;

/// Admit a private-world reply against the expected pull.
///
/// Synthetic callers may use this to prove verifier behavior. A successful
/// admit is **not** a physical Mac Pass.
pub fn admit_probe_reply(
    expected: &ProbePull,
    reply: &ProbeReply,
    previously_seen_generations: &[u64],
) -> Result<AdmittedProbeReply, ClipboardProbeFailClosedReason> {
    if expected.generation == 0 || expected.epoch == 0 {
        return Err(ClipboardProbeFailClosedReason::MalformedReply);
    }
    if expected.world != ContentWorld::PrivateProbe
        || expected.path != ScriptEvaluationPath::PrivateContentWorldPull
    {
        return Err(ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation);
    }
    if reply.path != ScriptEvaluationPath::PrivateContentWorldPull
        || reply.world != ContentWorld::PrivateProbe
    {
        return Err(ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation);
    }
    if reply.generation != expected.generation {
        return Err(ClipboardProbeFailClosedReason::StaleGeneration);
    }
    if reply.epoch != expected.epoch {
        return Err(ClipboardProbeFailClosedReason::NavigationEpochDrift);
    }
    if previously_seen_generations.contains(&reply.generation) {
        return Err(ClipboardProbeFailClosedReason::DuplicateReply);
    }
    if reply.receipts.is_empty() {
        return Err(ClipboardProbeFailClosedReason::MissingReply);
    }

    let mut seen_ops = Vec::new();
    for receipt in &reply.receipts {
        if receipt.generation != expected.generation {
            return Err(ClipboardProbeFailClosedReason::StaleGeneration);
        }
        if receipt.epoch != expected.epoch {
            return Err(ClipboardProbeFailClosedReason::NavigationEpochDrift);
        }
        if receipt.host_pasteboard_touched {
            return Err(ClipboardProbeFailClosedReason::UnexpectedHostClipboardChange);
        }
        if !receipt.mediated_page_local {
            return Err(ClipboardProbeFailClosedReason::Uncertain);
        }
        if seen_ops.contains(&receipt.operation) {
            return Err(ClipboardProbeFailClosedReason::DuplicateReply);
        }
        seen_ops.push(receipt.operation);
    }

    for required in ClipboardOperation::ALL {
        if !seen_ops.contains(&required) {
            return Err(ClipboardProbeFailClosedReason::MissingReply);
        }
    }

    Ok(AdmittedProbeReply {
        generation: reply.generation,
        epoch: reply.epoch,
        receipts: reply.receipts.clone(),
    })
}

/// WKClipboardProbe component. Native WebKit exists only on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WKClipboardProbe;

impl WKClipboardProbe {
    pub fn webkit_available() -> bool {
        #[cfg(target_os = "macos")]
        {
            macos::webkit_available()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Execute one generation pull. Never evaluates into the page world.
    ///
    /// Non-macOS is deterministic unsupported. macOS missing WebKit, timeout,
    /// or permission prompts fail closed and never claim Pass.
    pub fn pull(expected: &ProbePull) -> HarnessResult<ProbeReply> {
        if expected.world != ContentWorld::PrivateProbe
            || expected.path != ScriptEvaluationPath::PrivateContentWorldPull
        {
            return Err(HarnessError::invalid_state(
                "WKClipboardProbe refuses page-world evaluateJavaScript and callAsyncJavaScript",
            ));
        }
        #[cfg(target_os = "macos")]
        {
            macos::pull(expected)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = expected;
            Err(HarnessError::backend_unavailable(
                "WKClipboardProbe is unsupported on non-macOS; WebKit private-world pull is unavailable",
            ))
        }
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn fail_closed_reason_from_harness(err: &HarnessError) -> ClipboardProbeFailClosedReason {
    let message = err.message.to_ascii_lowercase();
    if message.contains("unsupported on non-macos") {
        ClipboardProbeFailClosedReason::UnsupportedPlatform
    } else if message.contains("timeout") {
        ClipboardProbeFailClosedReason::Timeout
    } else if message.contains("permission") || message.contains("prompt") {
        ClipboardProbeFailClosedReason::PermissionPrompt
    } else if message.contains("webkit") || message.contains("content world") {
        ClipboardProbeFailClosedReason::MissingWebKitCapability
    } else if message.contains("evaluatejavascript") || message.contains("callasync") {
        ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation
    } else {
        ClipboardProbeFailClosedReason::Uncertain
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::ffi::CString;
    use std::time::Instant;

    pub fn webkit_available() -> bool {
        webkit_loaded()
            && AnyClass::get(c"WKWebView").is_some()
            && AnyClass::get(c"WKWebViewConfiguration").is_some()
            && AnyClass::get(c"WKContentWorld").is_some()
            && AnyClass::get(c"WKUserScript").is_some()
    }

    pub fn pull(expected: &ProbePull) -> HarnessResult<ProbeReply> {
        if !webkit_available() {
            return Err(HarnessError::backend_unavailable(
                "WKClipboardProbe missing WebKit capability (WKWebView/WKContentWorld)",
            ));
        }
        if ns_app_activation_policy_regular_would_prompt() {
            return Err(HarnessError::backend_unavailable(
                "WKClipboardProbe refuses permission prompt paths",
            ));
        }

        let html = nsstring(PROBE_FIXTURE_HTML).ok_or_else(|| {
            HarnessError::backend_unavailable("WKClipboardProbe fixture HTML NSString unavailable")
        })?;
        let page_source = nsstring(PAGE_WORLD_INTERCEPTOR_SOURCE).ok_or_else(|| {
            HarnessError::backend_unavailable(
                "WKClipboardProbe page interceptor NSString unavailable",
            )
        })?;
        let private_source = format!(
            "{PRIVATE_WORLD_PULL_SOURCE}\ngrokptahClipboardPull({}, {});",
            expected.generation, expected.epoch
        );
        let private_source = nsstring(&private_source).ok_or_else(|| {
            HarnessError::backend_unavailable("WKClipboardProbe private pull NSString unavailable")
        })?;
        let world_name = nsstring(PRIVATE_PROBE_WORLD_NAME).ok_or_else(|| {
            HarnessError::backend_unavailable("WKClipboardProbe world name NSString unavailable")
        })?;

        let config_cls = AnyClass::get(c"WKWebViewConfiguration").ok_or_else(|| {
            HarnessError::backend_unavailable("WKWebViewConfiguration unavailable")
        })?;
        let config: Retained<AnyObject> = unsafe { objc2::msg_send![config_cls, new] };

        let world_cls = AnyClass::get(c"WKContentWorld")
            .ok_or_else(|| HarnessError::backend_unavailable("WKContentWorld unavailable"))?;
        let private_world: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![world_cls, worldWithName: &*world_name] };
        let private_world = private_world.ok_or_else(|| {
            HarnessError::backend_unavailable("WKContentWorld.worldWithName unavailable")
        })?;
        let page_world: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![world_cls, pageWorld] };
        let page_world = page_world.ok_or_else(|| {
            HarnessError::backend_unavailable("WKContentWorld.pageWorld unavailable")
        })?;

        let user_script_cls = AnyClass::get(c"WKUserScript")
            .ok_or_else(|| HarnessError::backend_unavailable("WKUserScript unavailable"))?;
        // WKUserScriptInjectionTimeAtDocumentStart = 0; AtDocumentEnd = 1
        let page_script: Retained<AnyObject> = unsafe { objc2::msg_send![user_script_cls, alloc] };
        let page_script: Retained<AnyObject> = unsafe {
            objc2::msg_send![
                &*page_script,
                initWithSource: &*page_source,
                injectionTime: 0usize,
                forMainFrameOnly: true,
                inContentWorld: &*page_world
            ]
        };
        let private_script: Retained<AnyObject> =
            unsafe { objc2::msg_send![user_script_cls, alloc] };
        let private_script: Retained<AnyObject> = unsafe {
            objc2::msg_send![
                &*private_script,
                initWithSource: &*private_source,
                injectionTime: 1usize,
                forMainFrameOnly: true,
                inContentWorld: &*private_world
            ]
        };

        let controller: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![&*config, userContentController] };
        let controller = controller.ok_or_else(|| {
            HarnessError::backend_unavailable("WKUserContentController unavailable")
        })?;
        let _: () = unsafe { objc2::msg_send![&*controller, addUserScript: &*page_script] };
        let _: () = unsafe { objc2::msg_send![&*controller, addUserScript: &*private_script] };

        let webview_cls = AnyClass::get(c"WKWebView")
            .ok_or_else(|| HarnessError::backend_unavailable("WKWebView unavailable"))?;
        let frame = cg_rect_zero();
        let webview: Retained<AnyObject> = unsafe { objc2::msg_send![webview_cls, alloc] };
        let webview: Retained<AnyObject> =
            unsafe { objc2::msg_send![&*webview, initWithFrame: frame, configuration: &*config] };

        let _: () = unsafe {
            objc2::msg_send![&*webview, loadHTMLString: &*html, baseURL: std::ptr::null::<AnyObject>()]
        };

        // Private-world WKUserScript performs the pull. Never call
        // evaluateJavaScript / callAsyncJavaScript on the contained page.
        // Poll WKWebView.title for the private-world reply prefix.
        let started = Instant::now();
        loop {
            if started.elapsed() > PROBE_TIMEOUT {
                return Err(HarnessError::backend_unavailable(
                    "WKClipboardProbe private-world pull timeout",
                ));
            }
            pump_runloop_briefly();
            if let Some(json) = private_reply_from_title(&webview) {
                return parse_reply(&json);
            }
        }
    }

    fn private_reply_from_title(webview: &AnyObject) -> Option<String> {
        let title: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![webview, title] };
        let title = nsstring_to_string(title.as_deref())?;
        title
            .strip_prefix(PRIVATE_REPLY_TITLE_PREFIX)
            .map(str::to_owned)
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

    fn parse_reply(json: &str) -> HarnessResult<ProbeReply> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireReceipt {
            operation: String,
            generation: u64,
            epoch: u64,
            mediated_page_local: bool,
            host_pasteboard_touched: bool,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireReply {
            generation: u64,
            epoch: u64,
            world: String,
            path: String,
            receipts: Vec<WireReceipt>,
        }
        let wire: WireReply = serde_json::from_str(json).map_err(|_| {
            HarnessError::invalid_state("WKClipboardProbe private-world reply was malformed JSON")
        })?;
        let world = match wire.world.as_str() {
            "private_probe" => ContentWorld::PrivateProbe,
            "page" => ContentWorld::Page,
            _ => {
                return Err(HarnessError::invalid_state(
                    "WKClipboardProbe reply world is malformed",
                ))
            }
        };
        let path = match wire.path.as_str() {
            "private_content_world_pull" => ScriptEvaluationPath::PrivateContentWorldPull,
            "page_world_evaluate_javascript" => ScriptEvaluationPath::PageWorldEvaluateJavaScript,
            "call_async_javascript" => ScriptEvaluationPath::CallAsyncJavaScript,
            _ => {
                return Err(HarnessError::invalid_state(
                    "WKClipboardProbe reply path is malformed",
                ))
            }
        };
        let mut receipts = Vec::new();
        for item in wire.receipts {
            let operation = match item.operation.as_str() {
                "copy" => ClipboardOperation::Copy,
                "cut" => ClipboardOperation::Cut,
                "paste" => ClipboardOperation::Paste,
                "write" => ClipboardOperation::Write,
                _ => {
                    return Err(HarnessError::invalid_state(
                        "WKClipboardProbe receipt operation is malformed",
                    ))
                }
            };
            receipts.push(PageLocalClipboardReceipt {
                operation,
                generation: item.generation,
                epoch: item.epoch,
                mediated_page_local: item.mediated_page_local,
                host_pasteboard_touched: item.host_pasteboard_touched,
            });
        }
        Ok(ProbeReply {
            generation: wire.generation,
            epoch: wire.epoch,
            world,
            path,
            receipts,
        })
    }

    fn pump_runloop_briefly() {
        let cls = match AnyClass::get(c"NSRunLoop") {
            Some(cls) => cls,
            None => return,
        };
        let current: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![cls, currentRunLoop] };
        let Some(current) = current else {
            return;
        };
        let date_cls = match AnyClass::get(c"NSDate") {
            Some(cls) => cls,
            None => return,
        };
        let date: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![date_cls, dateWithTimeIntervalSinceNow: 0.05f64] };
        let Some(date) = date else {
            return;
        };
        let _: bool = unsafe {
            objc2::msg_send![&*current, runMode: &*nsstring("NSDefaultRunLoopMode").expect("mode"), beforeDate: &*date]
        };
    }

    fn ns_app_activation_policy_regular_would_prompt() -> bool {
        false
    }

    #[repr(C)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    fn cg_rect_zero() -> CGRect {
        CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_receipts(generation: u64, epoch: u64) -> Vec<PageLocalClipboardReceipt> {
        ClipboardOperation::ALL
            .into_iter()
            .map(|operation| PageLocalClipboardReceipt {
                operation,
                generation,
                epoch,
                mediated_page_local: true,
                host_pasteboard_touched: false,
            })
            .collect()
    }

    fn valid_reply(generation: u64, epoch: u64) -> ProbeReply {
        ProbeReply {
            generation,
            epoch,
            world: ContentWorld::PrivateProbe,
            path: ScriptEvaluationPath::PrivateContentWorldPull,
            receipts: valid_receipts(generation, epoch),
        }
    }

    #[test]
    fn private_world_reply_is_admitted() {
        let pull = ProbePull::private(3, 1);
        let admitted = admit_probe_reply(&pull, &valid_reply(3, 1), &[]).expect("admit");
        assert_eq!(admitted.generation, 3);
        assert_eq!(admitted.receipts.len(), 4);
    }

    #[test]
    fn page_world_evaluate_javascript_is_forbidden() {
        let mut pull = ProbePull::private(1, 1);
        pull.path = ScriptEvaluationPath::PageWorldEvaluateJavaScript;
        pull.world = ContentWorld::Page;
        let err = admit_probe_reply(&pull, &valid_reply(1, 1), &[]).expect_err("forbidden");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation
        );
    }

    #[test]
    fn call_async_javascript_path_is_forbidden() {
        let pull = ProbePull::private(1, 1);
        let mut reply = valid_reply(1, 1);
        reply.path = ScriptEvaluationPath::CallAsyncJavaScript;
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("forbidden");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation
        );
    }

    #[test]
    fn stale_generation_is_rejected() {
        let pull = ProbePull::private(4, 1);
        let err = admit_probe_reply(&pull, &valid_reply(3, 1), &[]).expect_err("stale");
        assert_eq!(err, ClipboardProbeFailClosedReason::StaleGeneration);
    }

    #[test]
    fn epoch_drift_is_rejected() {
        let pull = ProbePull::private(1, 2);
        let err = admit_probe_reply(&pull, &valid_reply(1, 1), &[]).expect_err("epoch");
        assert_eq!(err, ClipboardProbeFailClosedReason::NavigationEpochDrift);
    }

    #[test]
    fn duplicate_generation_is_rejected() {
        let pull = ProbePull::private(1, 1);
        let err = admit_probe_reply(&pull, &valid_reply(1, 1), &[1]).expect_err("dup");
        assert_eq!(err, ClipboardProbeFailClosedReason::DuplicateReply);
    }

    #[test]
    fn missing_operation_is_rejected() {
        let pull = ProbePull::private(1, 1);
        let mut reply = valid_reply(1, 1);
        reply.receipts.pop();
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("missing");
        assert_eq!(err, ClipboardProbeFailClosedReason::MissingReply);
    }

    #[test]
    fn non_macos_pull_is_unsupported() {
        let result = WKClipboardProbe::pull(&ProbePull::private(1, 1));
        #[cfg(not(target_os = "macos"))]
        {
            let err = result.expect_err("linux");
            assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
            assert!(!WKClipboardProbe::webkit_available());
            assert_eq!(
                fail_closed_reason_from_harness(&err),
                ClipboardProbeFailClosedReason::UnsupportedPlatform
            );
        }
        #[cfg(target_os = "macos")]
        {
            let _ = result;
        }
    }
}
