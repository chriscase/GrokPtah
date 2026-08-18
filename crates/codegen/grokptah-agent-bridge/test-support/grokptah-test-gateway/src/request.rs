//! Bounded wire-level capture of one HTTP/1.1 request.

use std::fmt;

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use crate::redacted_headers;

const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// One request exactly as the loopback listener observed it.
///
/// Header order and duplicate names are preserved. Raw header values remain
/// accessible for assertions, but the custom [`Debug`] implementation
/// redacts credential-like headers and never renders the body.
#[derive(Clone, Default)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Monotonic connection acceptance order, starting at zero.
    pub seq: usize,
}

impl fmt::Debug for RecordedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordedRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("headers", &redacted_headers(&self.headers))
            .field("body_len", &self.body.len())
            .field("seq", &self.seq)
            .finish()
    }
}

impl RecordedRequest {
    /// Case-insensitive first-match lookup. The returned value is raw and is
    /// intended for direct assertions, not logging.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn body_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    pub fn json_body(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// Read a fixed-length HTTP/1.1 request. Chunked request bodies are outside
/// this provider-test harness's scope; GrokPtah sends fixed JSON requests.
pub(crate) async fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];

    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return None;
        }
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]);
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();

    let mut headers = Vec::new();
    let mut content_length = 0_usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_owned();
        let value = value.trim().to_owned();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().ok()?;
        }
        headers.push((name, value));
    }
    if content_length > MAX_BODY_BYTES {
        return None;
    }

    let body_start = head_end + 4;
    while buffer.len() < body_start + content_length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    Some(RecordedRequest {
        method,
        path,
        headers,
        body: buffer[body_start..body_start + content_length].to_vec(),
        seq: 0,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_auth_and_omits_body() {
        let request = RecordedRequest {
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers: vec![
                ("Authorization".into(), "Bearer never-print-me".into()),
                ("content-type".into(), "application/json".into()),
            ],
            body: b"body-secret-never-print-me".to_vec(),
            seq: 7,
        };
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("never-print-me"));
        assert!(!rendered.contains("body-secret"));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("body_len"));
    }
}
