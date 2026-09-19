//! Contained Browser v0 page-containment policy.
//!
//! Exact owned-page allowlist, main-frame DOM only, a fresh nonpersistent
//! website-data store per run, and native deny of downloads / file pickers /
//! popups / new windows. Shared by the in-process simulator and live WK so the
//! two paths cannot diverge. Not isolation PASS, not Computer Mode.

use uuid::Uuid;

use crate::error::{HarnessError, HarnessResult};
use crate::simulator::GuestLocalAction;

/// Exact origin for the single owned fixture page.
pub const OWNED_PAGE_ORIGIN: &str = "https://grokptah.owned.invalid";
/// Exact path of the owned fixture page.
pub const OWNED_PAGE_PATH: &str = "/cb-v0/";
/// Exact URL admitted by the v0 allowlist.
pub const OWNED_PAGE_URL: &str = "https://grokptah.owned.invalid/cb-v0/";
/// Custom scheme that live WK registers a `WKURLSchemeHandler` for so the
/// download probe can deliver `Content-Disposition: attachment` + octet-stream.
/// Not on the page allowlist; never a navigable document.
pub const DOWNLOAD_PROBE_SCHEME: &str = "grokptah-cbv0";
/// Dedicated download URL served by the live-WK scheme handler.
pub const DOWNLOAD_PROBE_URL: &str = "grokptah-cbv0://owned/deny.bin";
/// Filename the download probe must never write to disk.
pub const DOWNLOAD_PROBE_FILENAME: &str = "deny.bin";

/// True when `url` is the dedicated download probe (scheme, owned-path `.bin`,
/// or a same-origin `blob:` created from the owned page). Not a page admit.
pub fn is_download_probe_url(url: &str) -> bool {
    url == DOWNLOAD_PROBE_URL
        || url.starts_with("grokptah-cbv0:")
        || url.contains("/cb-v0/deny.bin")
        || (url.starts_with("blob:") && url.contains("grokptah.owned.invalid"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    MainFrame,
    SecondaryWindow,
    BlankTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionChannel {
    MainFrameDom,
    HostKeyboard,
    HostPointer,
    HostClipboard,
}

/// Native WK capabilities that Contained Browser v0 always refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDenyKind {
    Download,
    FilePicker,
    DirectoryPicker,
    WindowOpen,
    Popup,
    NewWindow,
}

impl NativeDenyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::FilePicker => "file_picker",
            Self::DirectoryPicker => "directory_picker",
            Self::WindowOpen => "window_open",
            Self::Popup => "popup",
            Self::NewWindow => "new_window",
        }
    }
}

/// Process-local token for a nonpersistent website-data store. A new token is
/// minted every boot; runs cannot share or import profile/credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonpersistentWebsiteDataStore {
    run_id: String,
}

impl NonpersistentWebsiteDataStore {
    pub fn mint() -> Self {
        Self {
            run_id: Uuid::new_v4().to_string(),
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

/// Admit navigation only to the exact owned fixture page.
pub fn admit_navigation(url: &str) -> HarnessResult<()> {
    if url == OWNED_PAGE_URL {
        Ok(())
    } else {
        Err(HarnessError::invalid_state(format!(
            "off-allowlist navigation denied: {url} (owned page is {OWNED_PAGE_URL})"
        )))
    }
}

/// URL the simulator and live WK both arm at boot. Fail-closed unless admitted.
pub fn owned_page_for_boot() -> HarnessResult<&'static str> {
    admit_navigation(OWNED_PAGE_URL)?;
    Ok(OWNED_PAGE_URL)
}

/// Live WK navigation-policy helper used by `WKNavigationDelegate`.
/// `_blank` / non-main-frame / off-allowlist / downloads are cancelled.
pub fn live_wk_navigation_policy_allows(
    url: &str,
    is_main_frame: bool,
    is_blank_target: bool,
) -> bool {
    live_wk_navigation_action_policy_allows(url, is_main_frame, is_blank_target, false)
}

/// Same as [`live_wk_navigation_policy_allows`], plus `shouldPerformDownload`.
pub fn live_wk_navigation_action_policy_allows(
    url: &str,
    is_main_frame: bool,
    is_blank_target: bool,
    should_perform_download: bool,
) -> bool {
    if should_perform_download || is_blank_target || !is_main_frame {
        return false;
    }
    admit_navigation(url).is_ok()
}

/// `WKNavigationResponse` policy: allow only the owned main-frame document.
/// Download MIME / attachment / non-main-frame fail closed (cancel, never Download).
pub fn live_wk_navigation_response_policy_allows(
    url: &str,
    is_main_frame: bool,
    can_show_mime: bool,
    is_download_response: bool,
) -> bool {
    if is_download_response || !can_show_mime || !is_main_frame {
        return false;
    }
    admit_navigation(url).is_ok()
}

/// Downloads are never admitted. Native `WKDownload` / response-policy deny.
pub fn live_wk_download_policy_allows() -> bool {
    false
}

/// File and directory pickers are never admitted (`runOpenPanel` → nil URLs).
/// Live WK must deliver `WKOpenPanelParameters`; IMP pokes are not a deny.
pub fn live_wk_open_panel_policy_allows(_allows_directories: bool) -> bool {
    false
}

/// `window.open` / popups / new windows are never admitted (`createWebView` → nil).
pub fn live_wk_create_webview_policy_allows() -> bool {
    false
}

/// Shared fail-closed gate for native capabilities the live WK delegates deny.
pub fn admit_native_capability(kind: NativeDenyKind) -> HarnessResult<()> {
    Err(HarnessError::invalid_state(format!(
        "contained browser v0 native deny: {} is refused",
        kind.as_str()
    )))
}

/// Admit only main-frame DOM actions. Secondary windows, `_blank`, and host
/// keyboard / pointer / clipboard claims are refused.
pub fn admit_frame_action(frame: FrameKind, channel: ActionChannel) -> HarnessResult<()> {
    match (frame, channel) {
        (FrameKind::MainFrame, ActionChannel::MainFrameDom) => Ok(()),
        (FrameKind::BlankTarget, _) => Err(HarnessError::invalid_state(
            "secondary navigation target _blank is refused",
        )),
        (FrameKind::SecondaryWindow, _) => {
            Err(HarnessError::invalid_state("secondary windows are refused"))
        }
        (_, ActionChannel::HostKeyboard) => Err(HarnessError::invalid_state(
            "host keyboard claims are refused",
        )),
        (_, ActionChannel::HostPointer) => Err(HarnessError::invalid_state(
            "host pointer claims are refused",
        )),
        (_, ActionChannel::HostClipboard) => Err(HarnessError::invalid_state(
            "host clipboard claims are refused",
        )),
    }
}

/// Click / type are the only guest-local DOM actions in v0.
pub fn admit_guest_local_action(action: GuestLocalAction) -> HarnessResult<()> {
    match action {
        GuestLocalAction::ClickGuestButton | GuestLocalAction::TypeGuestText => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contained_browser_owned_page_is_admitted_off_allowlist_is_denied() {
        assert_eq!(owned_page_for_boot().expect("owned page"), OWNED_PAGE_URL);
        admit_navigation("https://example.com/").expect_err("off-allowlist");
        admit_navigation("https://grokptah.owned.invalid/other")
            .expect_err("same origin other path");
        admit_navigation("about:blank").expect_err("about:blank");
        admit_navigation(DOWNLOAD_PROBE_URL).expect_err("download probe is not a page");
        assert!(is_download_probe_url(DOWNLOAD_PROBE_URL));
        assert!(is_download_probe_url(
            "https://grokptah.owned.invalid/cb-v0/deny.bin"
        ));
        assert!(is_download_probe_url(
            "blob:https://grokptah.owned.invalid/cb-v0/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        ));
        assert!(!is_download_probe_url(OWNED_PAGE_URL));
    }

    #[test]
    fn contained_browser_main_frame_dom_admitted_blank_and_host_refused() {
        admit_frame_action(FrameKind::MainFrame, ActionChannel::MainFrameDom).expect("main DOM");
        admit_frame_action(FrameKind::BlankTarget, ActionChannel::MainFrameDom)
            .expect_err("_blank");
        admit_frame_action(FrameKind::SecondaryWindow, ActionChannel::MainFrameDom)
            .expect_err("secondary");
        admit_frame_action(FrameKind::MainFrame, ActionChannel::HostKeyboard)
            .expect_err("keyboard");
        admit_frame_action(FrameKind::MainFrame, ActionChannel::HostPointer).expect_err("pointer");
        admit_frame_action(FrameKind::MainFrame, ActionChannel::HostClipboard)
            .expect_err("clipboard");
    }

    #[test]
    fn contained_browser_nonpersistent_store_tokens_are_unique_per_mint() {
        let a = NonpersistentWebsiteDataStore::mint();
        let b = NonpersistentWebsiteDataStore::mint();
        assert_ne!(a.run_id(), b.run_id());
    }

    #[test]
    fn contained_browser_live_wk_navigation_policy_cancels_blank_and_off_allowlist() {
        assert!(live_wk_navigation_policy_allows(
            OWNED_PAGE_URL,
            true,
            false
        ));
        assert!(!live_wk_navigation_policy_allows(
            OWNED_PAGE_URL,
            true,
            true
        ));
        assert!(!live_wk_navigation_policy_allows(
            OWNED_PAGE_URL,
            false,
            false
        ));
        assert!(!live_wk_navigation_policy_allows(
            "https://evil.example/",
            true,
            false
        ));
        assert!(!live_wk_navigation_action_policy_allows(
            OWNED_PAGE_URL,
            true,
            false,
            true
        ));
        assert!(live_wk_navigation_response_policy_allows(
            OWNED_PAGE_URL,
            true,
            true,
            false
        ));
        assert!(!live_wk_navigation_response_policy_allows(
            OWNED_PAGE_URL,
            true,
            false,
            false
        ));
        assert!(!live_wk_navigation_response_policy_allows(
            OWNED_PAGE_URL,
            true,
            true,
            true
        ));
    }

    #[test]
    fn contained_browser_native_deny_delegates_fail_closed() {
        assert!(!live_wk_download_policy_allows());
        assert!(!live_wk_open_panel_policy_allows(false));
        assert!(!live_wk_open_panel_policy_allows(true));
        assert!(!live_wk_create_webview_policy_allows());
        for kind in [
            NativeDenyKind::Download,
            NativeDenyKind::FilePicker,
            NativeDenyKind::DirectoryPicker,
            NativeDenyKind::WindowOpen,
            NativeDenyKind::Popup,
            NativeDenyKind::NewWindow,
        ] {
            admit_native_capability(kind).expect_err(kind.as_str());
        }
    }
}
