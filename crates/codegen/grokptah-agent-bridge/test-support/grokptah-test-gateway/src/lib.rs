//! Test-support-only scripted HTTP/1.1 gateway for GrokPtah provider tests.
//!
//! The gateway gives tests byte-level control that ordinary HTTP mocks do
//! not: ordered retries, path-routed concurrent responses, arbitrary stream
//! fragmentation and delay, short fixed-length bodies, connection resets,
//! and permanent stalls. It binds only to an ephemeral loopback port.
//!
//! Recorded requests retain raw values so tests can prove that production
//! clients sent the expected synthetic credentials. Their [`Debug`] output
//! is safe by default: authentication, cookie, token, secret, and API-key
//! header values are replaced with `<redacted>`, and request bodies are
//! represented by length rather than content. Use [`redact`] before placing
//! any other credential-bearing string in assertion or diagnostic output.
//!
//! # Example
//!
//! ```no_run
//! use grokptah_test_gateway::{MockGateway, Response, Step};
//!
//! # async fn example() {
//! let gateway = MockGateway::start_ordered(vec![
//!     Step::respond(Response::json_ok(&serde_json::json!({"ok": true}))),
//! ])
//! .await;
//!
//! let response = reqwest::get(format!("{}/v1/models", gateway.base_url()))
//!     .await
//!     .expect("request");
//! assert_eq!(response.status(), 200);
//! assert_eq!(gateway.requests()[0].path, "/v1/models");
//! # }
//! ```

mod redact;
mod request;
mod script;
mod server;

pub use redact::{is_sensitive_header, redact, redacted_headers};
pub use request::RecordedRequest;
pub use script::{split_at, split_evenly, Body, Frame, Response, Step};
pub use server::MockGateway;
