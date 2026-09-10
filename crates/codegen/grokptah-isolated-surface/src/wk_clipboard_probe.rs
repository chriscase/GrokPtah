//! WKClipboardProbe — page-world initiation + private-world pull protocol.
//!
//! The contained **page world** must initiate copy/cut/paste/write and bind
//! results to a host-issued generation/epoch challenge. The host never targets
//! the page with `evaluateJavaScript` or `callAsyncJavaScript`. Private world
//! may only read the already-produced bounded page result and forward it
//! through a registered `WKScriptMessageHandler` in that private world. It
//! must not initiate clipboard operations or construct success facts.
//!
//! Host receive is the private-world script-message handler only. Polling
//! `document.title`, DOM attribute self-attestation, or page-world evaluation
//! is untrusted and can never Pass. If a trustworthy page→private→host channel
//! cannot be registered, the probe is INCONCLUSIVE / fail closed.

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, HarnessResult};

/// Private content-world name. Must not collide with the page world.
pub const PRIVATE_PROBE_WORLD_NAME: &str = "grokptah.clipboard.probe.v1";

/// Script-message name registered only in the private world.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PRIVATE_REPLY_HANDLER_NAME: &str = "grokptahClipboardReply";

/// Shared DOM mailbox written by **page world** after it initiates operations.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PAGE_RESULT_MAILBOX_ID: &str = "grokptah-clipboard-probe-mailbox";

/// Attribute holding the bounded page-world JSON result. Private world may
/// read this attribute; it is not a host receive channel.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PAGE_RESULT_ATTRIBUTE: &str = "data-grokptah-page-result";

/// Maximum wait for a private-world handler message.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_000);

/// Hard cap on the handler body. Oversized replies fail closed.
pub const MAX_PROBE_REPLY_BYTES: usize = 4096;

/// Exact receipt count required for mediation proof.
pub const MAX_RECEIPT_COUNT: usize = ClipboardOperation::ALL.len();

/// Forbidden title-prefix marker. Host must never poll this as a reply channel.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PRIVATE_REPLY_TITLE_PREFIX: &str = "GROKPTAH-CLIPBOARD-REPLY:";

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

/// Who initiated a clipboard operation / constructed a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptInitiator {
    PageWorld,
    PrivateWorld,
}

/// How the host actually received bytes. Only the private-world script
/// message handler may ever seal Pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyChannel {
    PrivateWorldScriptMessageHandler,
    DocumentTitle,
    DomAttribute,
    EvaluateJavaScript,
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
    UntrustedReplyChannel,
    PrivateWorldGeneratedReceipts,
    PageWorldNonparticipation,
    OversizedReply,
    UnknownWireField,
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
    pub initiator: ReceiptInitiator,
}

/// Bounded page-world result. This is the only success-fact document; private
/// world must forward it verbatim and must not mint it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageWorldResult {
    pub generation: u64,
    pub epoch: u64,
    pub initiator: ReceiptInitiator,
    pub receipts: Vec<PageLocalClipboardReceipt>,
}

/// Private-world reply after the host attaches receive-path provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReply {
    pub generation: u64,
    pub epoch: u64,
    pub world: ContentWorld,
    pub path: ScriptEvaluationPath,
    pub reply_channel: ReplyChannel,
    pub page_world_participated: bool,
    pub private_world_initiated_operations: bool,
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

/// Page-world user script: intercept copy/cut/paste/write into a **page-world
/// JS store**. Never writes the host-bound mailbox and never invokes host
/// pasteboard APIs. Installed as a WKUserScript at document-start in the page
/// world (not via evaluateJavaScript).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PAGE_WORLD_INTERCEPTOR_SOURCE: &str = r#"
(function () {
  if (window.__grokptahClipboardInterceptorInstalled) { return; }
  window.__grokptahClipboardInterceptorInstalled = true;
  window.__grokptahClipboardOps = [];
  function record(op) {
    window.__grokptahClipboardOps.push(op);
  }
  document.addEventListener("copy", function (e) {
    e.preventDefault();
    record("copy");
  }, true);
  document.addEventListener("cut", function (e) {
    e.preventDefault();
    record("cut");
  }, true);
  document.addEventListener("paste", function (e) {
    e.preventDefault();
    record("paste");
  }, true);
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText = function (text) {
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

/// Private-world pull script. Host injects this **only** in the private
/// content world. It may read the page mailbox attribute and post the blob
/// to the registered handler. It must not initiate clipboard operations, stamp
/// receipts, or write `document.title`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PRIVATE_WORLD_PULL_SOURCE: &str = r#"
(function () {
  if (window.__grokptahClipboardPrivatePullInstalled) { return; }
  window.__grokptahClipboardPrivatePullInstalled = true;
  var posted = false;
  function postMailbox() {
    if (posted) { return; }
    posted = true;
    var blob = "";
    var el = document.getElementById("grokptah-clipboard-probe-mailbox");
    if (el) {
      var attr = el.getAttribute("data-grokptah-page-result");
      if (attr) { blob = String(attr); }
    }
    var handlers = window.webkit && window.webkit.messageHandlers;
    var handler = handlers && handlers.grokptahClipboardReply;
    if (handler && handler.postMessage) {
      handler.postMessage(blob);
    }
  }
  var tries = 0;
  (function poll() {
    var el = document.getElementById("grokptah-clipboard-probe-mailbox");
    var ready = el && el.getAttribute("data-grokptah-page-result");
    if (ready || tries >= 50) {
      postMailbox();
      return;
    }
    tries += 1;
    setTimeout(poll, 20);
  })();
})();
"#;

/// Page-world initiator. Host bakes the unforgeable generation/epoch challenge
/// into this script. Page world performs the four actual clipboard attempts
/// and writes the bounded mailbox result. Private world must not run this.
pub fn page_world_initiator_source(generation: u64, epoch: u64) -> String {
    format!(
        r#"
(function () {{
  var generation = {generation};
  var epoch = {epoch};
  var field = document.getElementById("probe-field");
  if (field) {{
    field.focus();
    field.select();
  }}
  try {{ document.execCommand("copy"); }} catch (e) {{}}
  try {{ document.execCommand("cut"); }} catch (e) {{}}
  try {{ document.execCommand("paste"); }} catch (e) {{}}
  if (navigator.clipboard && navigator.clipboard.writeText) {{
    try {{ navigator.clipboard.writeText("page-local-write"); }} catch (e) {{}}
  }}
  var ops = window.__grokptahClipboardOps || [];
  var seen = {{}};
  var receipts = [];
  for (var i = 0; i < ops.length; i++) {{
    var op = ops[i];
    if (seen[op]) {{ continue; }}
    seen[op] = true;
    receipts.push({{
      operation: op,
      generation: generation,
      epoch: epoch,
      mediatedPageLocal: true,
      hostPasteboardTouched: false,
      initiator: "page_world"
    }});
  }}
  var payload = {{
    generation: generation,
    epoch: epoch,
    initiator: "page_world",
    receipts: receipts
  }};
  var el = document.getElementById("grokptah-clipboard-probe-mailbox");
  if (!el) {{
    el = document.createElement("div");
    el.id = "grokptah-clipboard-probe-mailbox";
    el.setAttribute("hidden", "hidden");
    (document.documentElement || document.body).appendChild(el);
  }}
  el.setAttribute("data-grokptah-page-result", JSON.stringify(payload));
}})();
"#
    )
}

/// Disposable fixture HTML. No network, no auth, no private data.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const PROBE_FIXTURE_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>GrokPtah clipboard kill-gate</title>
<input id="probe-field" value="page-local-probe-text">
"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WirePageResult {
    generation: u64,
    epoch: u64,
    initiator: ReceiptInitiator,
    receipts: Vec<WireReceipt>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireReceipt {
    operation: ClipboardOperation,
    generation: u64,
    epoch: u64,
    mediated_page_local: bool,
    host_pasteboard_touched: bool,
    initiator: ReceiptInitiator,
}

fn map_wire_decode_error(err: &serde_json::Error) -> ClipboardProbeFailClosedReason {
    let message = err.to_string();
    if message.contains("unknown field") {
        ClipboardProbeFailClosedReason::UnknownWireField
    } else {
        ClipboardProbeFailClosedReason::MalformedReply
    }
}

/// Strict bounded decoder for the page-world mailbox blob.
///
/// Deny-unknown-fields, max bytes, max receipt count. Empty/missing blobs are
/// page-world nonparticipation, not a Pass. This function never treats DOM
/// attributes or `document.title` as an admitted host channel.
pub fn decode_page_result_bytes(
    bytes: &[u8],
) -> Result<PageWorldResult, ClipboardProbeFailClosedReason> {
    if bytes.len() > MAX_PROBE_REPLY_BYTES {
        return Err(ClipboardProbeFailClosedReason::OversizedReply);
    }
    let trimmed = std::str::from_utf8(bytes)
        .map_err(|_| ClipboardProbeFailClosedReason::MalformedReply)?
        .trim();
    if trimmed.is_empty() {
        return Err(ClipboardProbeFailClosedReason::PageWorldNonparticipation);
    }
    let wire: WirePageResult =
        serde_json::from_str(trimmed).map_err(|err| map_wire_decode_error(&err))?;
    if wire.receipts.len() > MAX_RECEIPT_COUNT {
        return Err(ClipboardProbeFailClosedReason::MalformedReply);
    }
    if wire.initiator != ReceiptInitiator::PageWorld {
        return Err(ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts);
    }
    let mut receipts = Vec::with_capacity(wire.receipts.len());
    for item in wire.receipts {
        if item.initiator != ReceiptInitiator::PageWorld {
            return Err(ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts);
        }
        receipts.push(PageLocalClipboardReceipt {
            operation: item.operation,
            generation: item.generation,
            epoch: item.epoch,
            mediated_page_local: item.mediated_page_local,
            host_pasteboard_touched: item.host_pasteboard_touched,
            initiator: item.initiator,
        });
    }
    Ok(PageWorldResult {
        generation: wire.generation,
        epoch: wire.epoch,
        initiator: wire.initiator,
        receipts,
    })
}

/// Attach host receive-path provenance. The page result itself cannot attest
/// the channel; only the host knows how bytes arrived.
pub fn probe_reply_from_page_result(
    page: PageWorldResult,
    reply_channel: ReplyChannel,
) -> ProbeReply {
    let page_world_participated =
        page.initiator == ReceiptInitiator::PageWorld && !page.receipts.is_empty();
    let private_world_initiated_operations = page.initiator == ReceiptInitiator::PrivateWorld
        || page
            .receipts
            .iter()
            .any(|receipt| receipt.initiator == ReceiptInitiator::PrivateWorld);
    ProbeReply {
        generation: page.generation,
        epoch: page.epoch,
        world: ContentWorld::PrivateProbe,
        path: ScriptEvaluationPath::PrivateContentWorldPull,
        reply_channel,
        page_world_participated,
        private_world_initiated_operations,
        receipts: page.receipts,
    }
}

/// Admit handler bodies received on the private-world script-message handler.
/// Zero messages is missing; two or more is duplicate. Title/DOM channels are
/// not modeled here — callers that did not use the handler must not call this.
pub fn admit_private_world_handler_messages(
    expected: &ProbePull,
    messages: &[Vec<u8>],
    previously_seen_generations: &[u64],
) -> Result<AdmittedProbeReply, ClipboardProbeFailClosedReason> {
    match messages.len() {
        0 => Err(ClipboardProbeFailClosedReason::MissingReply),
        1 => {
            let page = decode_page_result_bytes(&messages[0])?;
            let reply =
                probe_reply_from_page_result(page, ReplyChannel::PrivateWorldScriptMessageHandler);
            admit_probe_reply(expected, &reply, previously_seen_generations)
        }
        _ => Err(ClipboardProbeFailClosedReason::DuplicateReply),
    }
}

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
    if reply.reply_channel != ReplyChannel::PrivateWorldScriptMessageHandler {
        return Err(ClipboardProbeFailClosedReason::UntrustedReplyChannel);
    }
    if reply.private_world_initiated_operations {
        return Err(ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts);
    }
    if !reply.page_world_participated {
        return Err(ClipboardProbeFailClosedReason::PageWorldNonparticipation);
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
        return Err(ClipboardProbeFailClosedReason::PageWorldNonparticipation);
    }
    if reply.receipts.len() > MAX_RECEIPT_COUNT {
        return Err(ClipboardProbeFailClosedReason::MalformedReply);
    }

    let mut seen_ops = Vec::new();
    for receipt in &reply.receipts {
        if receipt.generation != expected.generation {
            return Err(ClipboardProbeFailClosedReason::StaleGeneration);
        }
        if receipt.epoch != expected.epoch {
            return Err(ClipboardProbeFailClosedReason::NavigationEpochDrift);
        }
        if receipt.initiator != ReceiptInitiator::PageWorld {
            return Err(ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts);
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
    /// Non-macOS is deterministic unsupported. macOS missing WebKit, missing
    /// private-world handler registration, timeout, or permission prompts
    /// fail closed and never claim Pass.
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
    } else if message.contains("evaluatejavascript") || message.contains("callasync") {
        ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation
    } else if message.contains("untrusted reply")
        || message.contains("document.title")
        || message.contains("title-channel")
        || message.contains("dom attribute")
    {
        ClipboardProbeFailClosedReason::UntrustedReplyChannel
    } else if message.contains("private-world generated")
        || message.contains("private world generated")
    {
        ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts
    } else if message.contains("page-world did not participate")
        || message.contains("page world nonparticipation")
    {
        ClipboardProbeFailClosedReason::PageWorldNonparticipation
    } else if message.contains("oversized") {
        ClipboardProbeFailClosedReason::OversizedReply
    } else if message.contains("unknown field") || message.contains("unknown wire") {
        ClipboardProbeFailClosedReason::UnknownWireField
    } else if message.contains("webkit")
        || message.contains("content world")
        || message.contains("script message handler")
    {
        ClipboardProbeFailClosedReason::MissingWebKitCapability
    } else {
        ClipboardProbeFailClosedReason::Uncertain
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn harness_error_for_probe_reason(reason: ClipboardProbeFailClosedReason) -> HarnessError {
    match reason {
        ClipboardProbeFailClosedReason::Timeout => HarnessError::backend_unavailable(
            "WKClipboardProbe private-world handler timeout",
        ),
        ClipboardProbeFailClosedReason::UnsupportedPlatform => HarnessError::backend_unavailable(
            "WKClipboardProbe is unsupported on non-macOS; WebKit private-world pull is unavailable",
        ),
        ClipboardProbeFailClosedReason::MissingWebKitCapability => HarnessError::backend_unavailable(
            "WKClipboardProbe missing WebKit capability (WKScriptMessageHandler in private content world)",
        ),
        ClipboardProbeFailClosedReason::PermissionPrompt => HarnessError::backend_unavailable(
            "WKClipboardProbe refuses permission prompt paths",
        ),
        ClipboardProbeFailClosedReason::ForbiddenScriptEvaluation => HarnessError::invalid_state(
            "WKClipboardProbe refuses page-world evaluateJavaScript and callAsyncJavaScript",
        ),
        ClipboardProbeFailClosedReason::UntrustedReplyChannel => HarnessError::invalid_state(
            "WKClipboardProbe untrusted reply channel (document.title / DOM attribute / evaluateJavaScript)",
        ),
        ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts => {
            HarnessError::invalid_state("WKClipboardProbe private-world generated receipts")
        }
        ClipboardProbeFailClosedReason::PageWorldNonparticipation => {
            HarnessError::invalid_state("WKClipboardProbe page-world did not participate")
        }
        ClipboardProbeFailClosedReason::OversizedReply => {
            HarnessError::invalid_state("WKClipboardProbe oversized private-world handler reply")
        }
        ClipboardProbeFailClosedReason::UnknownWireField => {
            HarnessError::invalid_state("WKClipboardProbe unknown wire field")
        }
        ClipboardProbeFailClosedReason::DuplicateReply => HarnessError::invalid_state(
            "WKClipboardProbe duplicate private-world handler reply",
        ),
        ClipboardProbeFailClosedReason::StaleGeneration => {
            HarnessError::invalid_state("WKClipboardProbe stale generation challenge")
        }
        ClipboardProbeFailClosedReason::MalformedReply => {
            HarnessError::invalid_state("WKClipboardProbe private-world reply was malformed JSON")
        }
        ClipboardProbeFailClosedReason::MissingReply => {
            HarnessError::invalid_state("WKClipboardProbe missing private-world handler reply")
        }
        ClipboardProbeFailClosedReason::NavigationEpochDrift => {
            HarnessError::invalid_state("WKClipboardProbe navigation epoch drifted")
        }
        ClipboardProbeFailClosedReason::UnexpectedHostClipboardChange => {
            HarnessError::invalid_state("WKClipboardProbe unexpected host clipboard change")
        }
        ClipboardProbeFailClosedReason::Uncertain => {
            HarnessError::uncertain_outcome("WKClipboardProbe private-world pull is uncertain")
        }
    }
}

/// Host-issued generation that fits in JS MAX_SAFE_INTEGER so the page-world
/// challenge survives JSON round-trip without precision loss.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn host_issued_generation() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(1);
    let mixed = nanos ^ (std::process::id() as u64).wrapping_shl(17) ^ nanos.rotate_left(13);
    let safe = mixed & ((1u64 << 53) - 1);
    if safe == 0 {
        1
    } else {
        safe
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::encode::{Encode, Encoding};
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, ClassBuilder, NSObject, Sel};
    use objc2::{sel, ClassType};
    use std::ffi::CString;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    struct HandlerInbox {
        messages: Vec<Vec<u8>>,
    }

    fn inbox() -> &'static Mutex<HandlerInbox> {
        static INBOX: OnceLock<Mutex<HandlerInbox>> = OnceLock::new();
        INBOX.get_or_init(|| {
            Mutex::new(HandlerInbox {
                messages: Vec::new(),
            })
        })
    }

    fn reset_inbox() {
        if let Ok(mut guard) = inbox().lock() {
            guard.messages.clear();
        }
    }

    fn snapshot_inbox() -> Vec<Vec<u8>> {
        inbox()
            .lock()
            .map(|guard| guard.messages.clone())
            .unwrap_or_default()
    }

    unsafe extern "C-unwind" fn did_receive_script_message(
        _this: &AnyObject,
        _cmd: Sel,
        _controller: *mut AnyObject,
        message: &AnyObject,
    ) {
        let name_obj: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![message, name] };
        let name = nsstring_to_string(name_obj.as_deref());
        if name.as_deref() != Some(PRIVATE_REPLY_HANDLER_NAME) {
            return;
        }
        let body: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![message, body] };
        let Some(body) = body else {
            return;
        };
        let Some(bytes) = nsstring_to_bytes(&body) else {
            if let Ok(mut guard) = inbox().lock() {
                guard.messages.push(Vec::new());
            }
            return;
        };
        if let Ok(mut guard) = inbox().lock() {
            guard.messages.push(bytes);
        }
    }

    fn handler_class() -> Option<&'static AnyClass> {
        static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
        *CLASS.get_or_init(|| {
            if let Some(existing) = AnyClass::get(c"GrokptahClipboardReplyHandlerV1") {
                return Some(existing);
            }
            let mut builder =
                ClassBuilder::new(c"GrokptahClipboardReplyHandlerV1", NSObject::class())?;
            if let Some(protocol) = AnyProtocol::get(c"WKScriptMessageHandler") {
                let _ = builder.add_protocol(protocol);
            }
            unsafe {
                builder.add_method(
                    sel!(userContentController:didReceiveScriptMessage:),
                    did_receive_script_message as unsafe extern "C-unwind" fn(_, _, _, _),
                );
            }
            Some(builder.register())
        })
    }

    pub fn webkit_available() -> bool {
        webkit_loaded()
            && AnyClass::get(c"WKWebView").is_some()
            && AnyClass::get(c"WKWebViewConfiguration").is_some()
            && AnyClass::get(c"WKContentWorld").is_some()
            && AnyClass::get(c"WKUserScript").is_some()
            && AnyClass::get(c"WKUserContentController").is_some()
            && controller_supports_private_world_handler()
            && handler_class().is_some()
    }

    fn controller_supports_private_world_handler() -> bool {
        let Some(cls) = AnyClass::get(c"WKUserContentController") else {
            return false;
        };
        let responds: bool = unsafe {
            objc2::msg_send![
                cls,
                instancesRespondToSelector: sel!(addScriptMessageHandler:contentWorld:name:)
            ]
        };
        responds
    }

    pub fn pull(expected: &ProbePull) -> HarnessResult<ProbeReply> {
        if !webkit_available() {
            return Err(harness_error_for_probe_reason(
                ClipboardProbeFailClosedReason::MissingWebKitCapability,
            ));
        }
        if ns_app_activation_policy_regular_would_prompt() {
            return Err(harness_error_for_probe_reason(
                ClipboardProbeFailClosedReason::PermissionPrompt,
            ));
        }

        reset_inbox();

        let html = nsstring(PROBE_FIXTURE_HTML).ok_or_else(|| {
            HarnessError::backend_unavailable("WKClipboardProbe fixture HTML NSString unavailable")
        })?;
        let page_interceptor = nsstring(PAGE_WORLD_INTERCEPTOR_SOURCE).ok_or_else(|| {
            HarnessError::backend_unavailable(
                "WKClipboardProbe page interceptor NSString unavailable",
            )
        })?;
        let page_initiator = nsstring(&page_world_initiator_source(
            expected.generation,
            expected.epoch,
        ))
        .ok_or_else(|| {
            HarnessError::backend_unavailable(
                "WKClipboardProbe page initiator NSString unavailable",
            )
        })?;
        let private_source = nsstring(PRIVATE_WORLD_PULL_SOURCE).ok_or_else(|| {
            HarnessError::backend_unavailable("WKClipboardProbe private pull NSString unavailable")
        })?;
        let world_name = nsstring(PRIVATE_PROBE_WORLD_NAME).ok_or_else(|| {
            HarnessError::backend_unavailable("WKClipboardProbe world name NSString unavailable")
        })?;
        let handler_name = nsstring(PRIVATE_REPLY_HANDLER_NAME).ok_or_else(|| {
            HarnessError::backend_unavailable("WKClipboardProbe handler name NSString unavailable")
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

        let handler_cls = handler_class().ok_or_else(|| {
            harness_error_for_probe_reason(ClipboardProbeFailClosedReason::MissingWebKitCapability)
        })?;
        let handler: Retained<AnyObject> = unsafe { objc2::msg_send![handler_cls, new] };

        let controller: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![&*config, userContentController] };
        let controller = controller.ok_or_else(|| {
            HarnessError::backend_unavailable("WKUserContentController unavailable")
        })?;

        let responds: bool = unsafe {
            objc2::msg_send![
                &*controller,
                respondsToSelector: sel!(addScriptMessageHandler:contentWorld:name:)
            ]
        };
        if !responds {
            return Err(harness_error_for_probe_reason(
                ClipboardProbeFailClosedReason::MissingWebKitCapability,
            ));
        }
        let _: () = unsafe {
            objc2::msg_send![
                &*controller,
                addScriptMessageHandler: &*handler,
                contentWorld: &*private_world,
                name: &*handler_name
            ]
        };

        let user_script_cls = AnyClass::get(c"WKUserScript")
            .ok_or_else(|| HarnessError::backend_unavailable("WKUserScript unavailable"))?;
        // WKUserScriptInjectionTimeAtDocumentStart = 0; AtDocumentEnd = 1
        let interceptor_script =
            init_user_script(user_script_cls, &page_interceptor, 0, true, &page_world).ok_or_else(
                || {
                    HarnessError::backend_unavailable(
                        "WKUserScript page interceptor init unavailable",
                    )
                },
            )?;
        let initiator_script =
            init_user_script(user_script_cls, &page_initiator, 1, true, &page_world).ok_or_else(
                || {
                    HarnessError::backend_unavailable(
                        "WKUserScript page initiator init unavailable",
                    )
                },
            )?;
        let private_script =
            init_user_script(user_script_cls, &private_source, 1, true, &private_world)
                .ok_or_else(|| {
                    HarnessError::backend_unavailable("WKUserScript private pull init unavailable")
                })?;

        let _: () = unsafe { objc2::msg_send![&*controller, addUserScript: &*interceptor_script] };
        let _: () = unsafe { objc2::msg_send![&*controller, addUserScript: &*initiator_script] };
        let _: () = unsafe { objc2::msg_send![&*controller, addUserScript: &*private_script] };

        let webview_cls = AnyClass::get(c"WKWebView")
            .ok_or_else(|| HarnessError::backend_unavailable("WKWebView unavailable"))?;
        let frame = cg_rect_zero();
        let webview_alloc: Allocated<AnyObject> = unsafe { objc2::msg_send![webview_cls, alloc] };
        let webview: Option<Retained<AnyObject>> = unsafe {
            objc2::msg_send![webview_alloc, initWithFrame: frame, configuration: &*config]
        };
        let webview = webview.ok_or_else(|| {
            HarnessError::backend_unavailable("WKWebView initWithFrame unavailable")
        })?;

        let _: () = unsafe {
            objc2::msg_send![&*webview, loadHTMLString: &*html, baseURL: None::<&AnyObject>]
        };

        // Host receive is the private-world WKScriptMessageHandler. Never poll
        // WKWebView.title, never evaluateJavaScript / callAsyncJavaScript.
        let started = Instant::now();
        loop {
            if started.elapsed() > PROBE_TIMEOUT {
                let _: () = unsafe {
                    objc2::msg_send![
                        &*controller,
                        removeScriptMessageHandlerForName: &*handler_name,
                        contentWorld: &*private_world
                    ]
                };
                return Err(harness_error_for_probe_reason(
                    ClipboardProbeFailClosedReason::Timeout,
                ));
            }
            pump_runloop_briefly();
            let messages = snapshot_inbox();
            if !messages.is_empty() {
                let _: () = unsafe {
                    objc2::msg_send![
                        &*controller,
                        removeScriptMessageHandlerForName: &*handler_name,
                        contentWorld: &*private_world
                    ]
                };
                return admit_private_world_handler_messages(expected, &messages, &[])
                    .map_err(harness_error_for_probe_reason)
                    .map(|admitted| ProbeReply {
                        generation: admitted.generation,
                        epoch: admitted.epoch,
                        world: ContentWorld::PrivateProbe,
                        path: ScriptEvaluationPath::PrivateContentWorldPull,
                        reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
                        page_world_participated: true,
                        private_world_initiated_operations: false,
                        receipts: admitted.receipts,
                    });
            }
        }
    }

    fn init_user_script(
        cls: &AnyClass,
        source: &AnyObject,
        injection_time: isize,
        main_frame_only: bool,
        world: &AnyObject,
    ) -> Option<Retained<AnyObject>> {
        let allocated: Allocated<AnyObject> = unsafe { objc2::msg_send![cls, alloc] };
        unsafe {
            objc2::msg_send![
                allocated,
                initWithSource: source,
                injectionTime: injection_time,
                forMainFrameOnly: main_frame_only,
                inContentWorld: world
            ]
        }
    }

    fn nsstring_to_bytes(object: &AnyObject) -> Option<Vec<u8>> {
        nsstring_to_string(Some(object)).map(String::into_bytes)
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

    // CGFloat is f64 on 64-bit Apple platforms (this crate's macOS workers).
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
        const ENCODING: Encoding =
            Encoding::Struct("CGPoint", &[CGFloat::ENCODING, CGFloat::ENCODING]);
    }

    unsafe impl Encode for CGSize {
        const ENCODING: Encoding =
            Encoding::Struct("CGSize", &[CGFloat::ENCODING, CGFloat::ENCODING]);
    }

    unsafe impl Encode for CGRect {
        const ENCODING: Encoding =
            Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
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
                initiator: ReceiptInitiator::PageWorld,
            })
            .collect()
    }

    fn valid_page_json(generation: u64, epoch: u64) -> String {
        let receipts: Vec<_> = ClipboardOperation::ALL
            .into_iter()
            .map(|operation| {
                serde_json::json!({
                    "operation": operation,
                    "generation": generation,
                    "epoch": epoch,
                    "mediatedPageLocal": true,
                    "hostPasteboardTouched": false,
                    "initiator": "page_world",
                })
            })
            .collect();
        serde_json::json!({
            "generation": generation,
            "epoch": epoch,
            "initiator": "page_world",
            "receipts": receipts,
        })
        .to_string()
    }

    fn valid_reply(generation: u64, epoch: u64) -> ProbeReply {
        ProbeReply {
            generation,
            epoch,
            world: ContentWorld::PrivateProbe,
            path: ScriptEvaluationPath::PrivateContentWorldPull,
            reply_channel: ReplyChannel::PrivateWorldScriptMessageHandler,
            page_world_participated: true,
            private_world_initiated_operations: false,
            receipts: valid_receipts(generation, epoch),
        }
    }

    #[test]
    fn host_issued_generation_is_js_safe_and_nonzero() {
        let generation = host_issued_generation();
        assert!(generation > 0);
        assert!(generation < (1u64 << 53));
    }

    #[test]
    fn private_world_handler_reply_is_admitted() {
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
    fn private_world_generated_receipts_cannot_pass() {
        let pull = ProbePull::private(1, 1);
        let mut reply = valid_reply(1, 1);
        reply.receipts[0].initiator = ReceiptInitiator::PrivateWorld;
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("private receipts");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts
        );

        let mut minted = valid_reply(1, 1);
        minted.private_world_initiated_operations = true;
        let err = admit_probe_reply(&pull, &minted, &[]).expect_err("private initiated");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts
        );

        let forged = serde_json::json!({
            "generation": 1,
            "epoch": 1,
            "initiator": "private_world",
            "receipts": serde_json::from_str::<serde_json::Value>(&valid_page_json(1, 1)).unwrap()["receipts"],
        })
        .to_string();
        let err = decode_page_result_bytes(forged.as_bytes()).expect_err("private initiator");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::PrivateWorldGeneratedReceipts
        );
    }

    #[test]
    fn page_world_nonparticipation_cannot_pass() {
        let pull = ProbePull::private(1, 1);
        let err = decode_page_result_bytes(b"").expect_err("empty");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::PageWorldNonparticipation
        );

        let err = admit_private_world_handler_messages(&pull, &[b"".to_vec()], &[])
            .expect_err("empty handler");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::PageWorldNonparticipation
        );

        let mut reply = valid_reply(1, 1);
        reply.page_world_participated = false;
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("flag");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::PageWorldNonparticipation
        );

        let missing_mailbox = serde_json::json!({
            "generation": 1,
            "epoch": 1,
            "initiator": "page_world",
            "receipts": []
        })
        .to_string();
        let page = decode_page_result_bytes(missing_mailbox.as_bytes()).expect("empty receipts");
        let reply =
            probe_reply_from_page_result(page, ReplyChannel::PrivateWorldScriptMessageHandler);
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("no receipts");
        assert_eq!(
            err,
            ClipboardProbeFailClosedReason::PageWorldNonparticipation
        );
    }

    #[test]
    fn title_channel_replies_cannot_pass() {
        let pull = ProbePull::private(1, 1);
        let mut reply = valid_reply(1, 1);
        reply.reply_channel = ReplyChannel::DocumentTitle;
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("title");
        assert_eq!(err, ClipboardProbeFailClosedReason::UntrustedReplyChannel);

        reply.reply_channel = ReplyChannel::DomAttribute;
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("dom");
        assert_eq!(err, ClipboardProbeFailClosedReason::UntrustedReplyChannel);

        reply.reply_channel = ReplyChannel::EvaluateJavaScript;
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("eval");
        assert_eq!(err, ClipboardProbeFailClosedReason::UntrustedReplyChannel);
    }

    #[test]
    fn dom_attribute_tampering_cannot_pass() {
        let interceptor_list =
            r#"[{"operation":"copy","mediatedPageLocal":true,"hostPasteboardTouched":false}]"#;
        let err = decode_page_result_bytes(interceptor_list.as_bytes()).expect_err("list");
        assert!(
            err == ClipboardProbeFailClosedReason::MalformedReply
                || err == ClipboardProbeFailClosedReason::UnknownWireField
        );

        let mut tampered =
            serde_json::from_str::<serde_json::Value>(&valid_page_json(1, 1)).unwrap();
        tampered["forgedAttribute"] = serde_json::json!(true);
        let err = decode_page_result_bytes(tampered.to_string().as_bytes()).expect_err("extra");
        assert_eq!(err, ClipboardProbeFailClosedReason::UnknownWireField);

        let mut wrong_challenge =
            serde_json::from_str::<serde_json::Value>(&valid_page_json(1, 1)).unwrap();
        wrong_challenge["generation"] = serde_json::json!(99);
        let page = decode_page_result_bytes(wrong_challenge.to_string().as_bytes()).expect("shape");
        let reply =
            probe_reply_from_page_result(page, ReplyChannel::PrivateWorldScriptMessageHandler);
        let err = admit_probe_reply(&ProbePull::private(1, 1), &reply, &[]).expect_err("stale");
        assert_eq!(err, ClipboardProbeFailClosedReason::StaleGeneration);
    }

    #[test]
    fn extra_unknown_fields_cannot_pass() {
        let mut extra = serde_json::from_str::<serde_json::Value>(&valid_page_json(1, 1)).unwrap();
        extra["world"] = serde_json::json!("private_probe");
        extra["path"] = serde_json::json!("private_content_world_pull");
        let err = decode_page_result_bytes(extra.to_string().as_bytes()).expect_err("envelope");
        assert_eq!(err, ClipboardProbeFailClosedReason::UnknownWireField);

        let mut receipt_extra =
            serde_json::from_str::<serde_json::Value>(&valid_page_json(1, 1)).unwrap();
        receipt_extra["receipts"][0]["via"] = serde_json::json!("title");
        let err = decode_page_result_bytes(receipt_extra.to_string().as_bytes()).expect_err("via");
        assert_eq!(err, ClipboardProbeFailClosedReason::UnknownWireField);
    }

    #[test]
    fn oversized_reply_cannot_pass() {
        let oversized = vec![b'x'; MAX_PROBE_REPLY_BYTES + 1];
        let err = decode_page_result_bytes(&oversized).expect_err("size");
        assert_eq!(err, ClipboardProbeFailClosedReason::OversizedReply);
    }

    #[test]
    fn stale_challenge_and_duplicate_handler_messages_cannot_pass() {
        let pull = ProbePull::private(9, 2);
        let page = decode_page_result_bytes(valid_page_json(1, 2).as_bytes()).expect("page");
        let reply =
            probe_reply_from_page_result(page, ReplyChannel::PrivateWorldScriptMessageHandler);
        let err = admit_probe_reply(&pull, &reply, &[]).expect_err("stale");
        assert_eq!(err, ClipboardProbeFailClosedReason::StaleGeneration);

        let body = valid_page_json(1, 1).into_bytes();
        let err = admit_private_world_handler_messages(
            &ProbePull::private(1, 1),
            &[body.clone(), body],
            &[],
        )
        .expect_err("dup handler");
        assert_eq!(err, ClipboardProbeFailClosedReason::DuplicateReply);
    }

    #[test]
    fn handler_decode_admits_verbatim_page_blob_only() {
        let pull = ProbePull::private(1, 1);
        let admitted =
            admit_private_world_handler_messages(&pull, &[valid_page_json(1, 1).into_bytes()], &[])
                .expect("handler");
        assert_eq!(admitted.receipts.len(), 4);
        assert!(admitted
            .receipts
            .iter()
            .all(|receipt| receipt.initiator == ReceiptInitiator::PageWorld));
    }

    #[test]
    fn private_world_script_is_read_only_pull() {
        let source = PRIVATE_WORLD_PULL_SOURCE;
        assert!(!source.contains("execCommand"));
        assert!(!source.contains("writeText"));
        assert!(!source.contains("clipboard.write"));
        assert!(!source.contains("document.title"));
        assert!(!source.contains(PRIVATE_REPLY_TITLE_PREFIX));
        assert!(!source.contains("mediatedPageLocal"));
        assert!(!source.contains("initiator"));
        assert!(source.contains("postMessage"));
        assert!(source.contains(PRIVATE_REPLY_HANDLER_NAME));
        assert!(source.contains(PAGE_RESULT_ATTRIBUTE));
        assert!(source.contains(PAGE_RESULT_MAILBOX_ID));
    }

    #[test]
    fn page_world_script_initiates_four_ops_and_binds_challenge() {
        let interceptor = PAGE_WORLD_INTERCEPTOR_SOURCE;
        assert!(!interceptor.contains(PAGE_RESULT_ATTRIBUTE));
        assert!(!interceptor.contains("execCommand"));
        assert!(interceptor.contains("preventDefault"));

        let initiator = page_world_initiator_source(7, 3);
        assert!(initiator.contains("execCommand"));
        assert!(initiator.contains("writeText"));
        assert!(initiator.contains("var generation = 7;"));
        assert!(initiator.contains("var epoch = 3;"));
        assert!(initiator.contains("page_world"));
        assert!(initiator.contains(PAGE_RESULT_ATTRIBUTE));
        assert!(!initiator.contains("document.title"));
        assert!(!initiator.contains(PRIVATE_REPLY_TITLE_PREFIX));
        assert!(!initiator.contains("postMessage"));
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
