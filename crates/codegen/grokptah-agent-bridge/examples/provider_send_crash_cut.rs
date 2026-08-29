//! Crash-cut helper for the provider-send lattice (#478).
//!
//! Runs one bound provider send against a caller-supplied loopback URL and dies
//! — really dies, via `abort()`, with no unwinding and no destructors — at a
//! named point on the send path. The integration test then reopens the ledger
//! as a second process and checks what the first one left behind.
//!
//! An in-process test can simulate an interruption, but only a real kill proves
//! the durable ordering: that a record found at `Preparing` could not have put
//! bytes on the wire, and that anything from `Sending` onwards stays uncertain.
//!
//! Usage: `provider_send_crash_cut <ledger-root> <session> <base-url> <cut>`
//! Never talks to a real provider: `base-url` must be an HTTP loopback address.

use grokptah_agent_bridge::provider_send::{
    self, CallSiteFamily, CrashCut, CutAction, ProviderRequestSpec, ProviderSendContext,
    ResponseAccept, SendOrigin, WireDialect,
};
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, root, session, base_url, cut] = args.as_slice() else {
        eprintln!("usage: provider_send_crash_cut <ledger-root> <session> <base-url> <cut>");
        std::process::exit(2);
    };

    // A helper that could reach a real provider would make the whole matrix
    // untrustworthy, so refuse anything but an explicit loopback HTTP address.
    let parsed = reqwest::Url::parse(base_url).expect("valid URL");
    let loopback = parsed
        .host_str()
        .unwrap_or_default()
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback());
    assert!(
        parsed.scheme() == "http" && loopback && parsed.port().is_some(),
        "crash-cut helper only targets an explicit HTTP loopback address"
    );

    let cut = CrashCut::parse(cut).expect("known crash cut");
    let context = ProviderSendContext::for_root(
        root,
        "crash-cut-helper",
        session,
        SendOrigin::Desktop,
        CallSiteFamily::DesktopBuildRound,
    )
    .expect("ledger");

    let body = serde_json::json!({
        "model": "synthetic-model",
        "messages": [{"role": "user", "content": "synthetic crash-cut probe"}],
        "stream": false
    });

    provider_send::arm_crash_cut(cut, CutAction::Abort);

    let cancel = CancellationToken::new();
    let spec = ProviderRequestSpec {
        credentials: None,
        base_url,
        wire_model: "synthetic-model",
        dialect: WireDialect::OpenAiChatCompletions,
        credential_binding: None,
        body: &body,
        accept: ResponseAccept::Json,
        effort_header: None,
        request_timeout: std::time::Duration::from_secs(10),
        observation: None,
    };

    match provider_send::dispatch(&context, spec, &cancel).await {
        Ok(sent) => {
            let mut reader = sent.into_reader();
            match reader.read_to_string(&cancel).await {
                Ok(_) => {
                    let _ = reader.settle_completed(None, None, None, None);
                }
                Err(error) => {
                    let _ = reader.settle_uncertain(
                        error
                            .uncertainty()
                            .unwrap_or(provider_send::UncertaintyClass::TransportError),
                    );
                }
            }
        }
        Err(error) => {
            eprintln!("dispatch ended: {error}");
        }
    }

    // Reaching here means the cut did not fire. The test asserts on the exit
    // status, so say so loudly rather than passing silently.
    eprintln!("crash cut {} did not fire", cut.as_str());
    std::process::exit(3);
}
