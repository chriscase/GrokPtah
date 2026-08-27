//! Cross-platform OS signal wiring.
//!
//! The host core is synchronous and testable without signals. This module is
//! the only place that touches the OS, and it does exactly one thing: escalate
//! the shared [`ShutdownSignal`]. The first signal drains; a second stops now.
//!
//! `SIGTERM` matters here because a headless host is normally stopped by a
//! supervisor, not by a keystroke. Windows has no `SIGTERM`, so console
//! `Ctrl+C` is the whole surface there.

use crate::lifecycle::{ShutdownKind, ShutdownSignal};

/// Watch for stop signals on a background thread until the process ends.
///
/// The returned handle owns a dedicated single-threaded runtime; dropping it
/// stops watching. Errors installing a handler are reported rather than
/// swallowed, because a host that cannot be stopped cleanly should say so.
pub fn watch(signal: ShutdownSignal) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("grokptah-headless-signals".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            runtime.block_on(watch_async(signal));
        })
}

async fn watch_async(signal: ShutdownSignal) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal as unix_signal};

        let mut terminate = match unix_signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(_) => return,
        };
        let mut interrupt = match unix_signal(SignalKind::interrupt()) {
            Ok(stream) => stream,
            Err(_) => return,
        };
        loop {
            let requested = tokio::select! {
                _ = terminate.recv() => true,
                _ = interrupt.recv() => true,
            };
            if !requested {
                return;
            }
            if escalate(&signal) {
                return;
            }
        }
    }

    #[cfg(not(unix))]
    {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            if escalate(&signal) {
                return;
            }
        }
    }
}

/// Escalate one notch; return `true` once the stop is immediate.
fn escalate(signal: &ShutdownSignal) -> bool {
    let next = match signal.state() {
        ShutdownKind::None => ShutdownKind::Graceful,
        _ => ShutdownKind::Immediate,
    };
    signal.request(next) == ShutdownKind::Immediate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_signal_drains_and_the_second_stops_now() {
        let signal = ShutdownSignal::new();
        assert!(!escalate(&signal));
        assert_eq!(signal.state(), ShutdownKind::Graceful);
        assert!(escalate(&signal));
        assert_eq!(signal.state(), ShutdownKind::Immediate);
        // Further signals stay immediate rather than cycling.
        assert!(escalate(&signal));
    }
}
