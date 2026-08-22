//! Shared desktop/hosted black-box parity fixture v1.
//!
//! After launch the scenario talks only through `McpControlClient`
//! (`initialize` / `list_tools` / `call_tool` / `close_session`). Launch
//! handles are retained solely to stop and restart the same home.
//!
//! The driver is loaded through `#[path]` so unrelated integration targets
//! that `mod common` do not compile this fixture.

#[path = "common/shared_black_box_v1.rs"]
mod shared_black_box_v1;

#[tokio::test(start_paused = true, flavor = "current_thread")]
#[allow(clippy::await_holding_lock)]
async fn shared_black_box_v1_desktop_hosted_parity() {
    shared_black_box_v1::run_fixture().await;
}
