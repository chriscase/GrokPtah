use grokptah_agent_bridge::EventReceiver;
use tauri::{AppHandle, Emitter};

/// Forward bridge session updates to the webview as `session://update` events.
pub fn spawn_event_forwarder(app: AppHandle, mut rx: EventReceiver) {
    tauri::async_runtime::spawn(async move {
        while let Some(update) = rx.recv().await {
            let _ = app.emit("session://update", update);
        }
    });
}
