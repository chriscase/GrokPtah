//! Contained Browser v0 page-containment policy.
//!
//! Exact owned-page allowlist, main-frame DOM only, and a fresh nonpersistent
//! website-data store per run. Shared by the in-process simulator and live WK
//! so the two paths cannot diverge. Not isolation PASS, not Computer Mode.

use uuid::Uuid;

use crate::error::{HarnessError, HarnessResult};
use crate::simulator::GuestLocalAction;

/// Exact origin for the single owned fixture page.
pub const OWNED_PAGE_ORIGIN: &str = "https://grokptah.owned.invalid";
/// Exact path of the owned fixture page.
pub const OWNED_PAGE_PATH: &str = "/cb-v0/";
/// Exact URL admitted by the v0 allowlist.
pub const OWNED_PAGE_URL: &str = "https://grokptah.owned.invalid/cb-v0/";

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
}
