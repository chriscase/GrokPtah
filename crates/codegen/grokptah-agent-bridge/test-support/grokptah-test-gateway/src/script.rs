//! Response scripting types for one accepted connection.

use std::time::Duration;

/// One byte fragment and the delay applied immediately before writing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub data: Vec<u8>,
    pub delay: Duration,
}

impl Frame {
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: data.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn delayed(data: impl Into<Vec<u8>>, delay: Duration) -> Self {
        Self {
            data: data.into(),
            delay,
        }
    }
}

/// Exact response-body framing and failure behavior.
#[derive(Debug, Clone)]
pub enum Body {
    Empty,
    Fixed {
        data: Vec<u8>,
        delay: Duration,
    },
    /// Correct `Content-Length`, emitted through arbitrary writes.
    FixedFragments(Vec<Frame>),
    /// Declares `declared_len`, sends fewer bytes, then closes or resets.
    FixedThenDrop {
        declared_len: usize,
        sent: Vec<u8>,
        reset: bool,
    },
    /// Standards-compliant HTTP chunked transfer with a terminal zero chunk.
    Chunked(Vec<Frame>),
    /// Sends chunks without the terminal zero chunk, then closes or resets.
    ChunkedThenDrop {
        frames: Vec<Frame>,
        reset: bool,
    },
    /// Raw close-delimited bytes, suitable for arbitrarily fragmented SSE.
    Stream(Vec<Frame>),
    /// Sends headers and then holds the connection open indefinitely.
    NeverEnds,
}

/// HTTP status, headers, timing, and body behavior for one response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub header_delay: Duration,
    pub body: Body,
}

impl Response {
    pub fn new(status: u16, body: Body) -> Self {
        Self {
            status,
            headers: Vec::new(),
            header_delay: Duration::ZERO,
            body,
        }
    }

    pub fn status_only(status: u16) -> Self {
        Self::new(status, Body::Empty)
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::text(
            status,
            "application/json",
            serde_json::to_vec(value).expect("serializable JSON fixture"),
        )
    }

    pub fn json_ok(value: &serde_json::Value) -> Self {
        Self::json(200, value)
    }

    pub fn text(status: u16, content_type: &str, data: impl Into<Vec<u8>>) -> Self {
        Self::new(
            status,
            Body::Fixed {
                data: data.into(),
                delay: Duration::ZERO,
            },
        )
        .with_header("content-type", content_type)
    }

    pub fn sse_stream(frames: Vec<Frame>) -> Self {
        Self::new(200, Body::Stream(frames)).with_header("content-type", "text/event-stream")
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_header_delay(mut self, delay: Duration) -> Self {
        self.header_delay = delay;
        self
    }

    pub fn with_body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }
}

/// What to do after a complete request has been read and recorded.
#[derive(Debug, Clone)]
pub enum Step {
    Respond(Response),
    CloseBeforeHeaders { reset: bool },
    Stall,
}

impl Step {
    pub fn respond(response: Response) -> Self {
        Self::Respond(response)
    }

    pub fn close() -> Self {
        Self::CloseBeforeHeaders { reset: false }
    }

    pub fn reset() -> Self {
        Self::CloseBeforeHeaders { reset: true }
    }

    pub fn stall() -> Self {
        Self::Stall
    }
}

/// Split bytes at exact offsets, including inside UTF-8 and JSON tokens.
pub fn split_at(data: &[u8], offsets: &[usize]) -> Vec<Frame> {
    let mut frames = Vec::with_capacity(offsets.len() + 1);
    let mut start = 0;
    for &offset in offsets {
        assert!(
            offset >= start && offset <= data.len(),
            "split offset {offset} out of range (start={start}, len={})",
            data.len()
        );
        frames.push(Frame::new(data[start..offset].to_vec()));
        start = offset;
    }
    frames.push(Frame::new(data[start..].to_vec()));
    frames
}

/// Split into at most `count` roughly equal frames.
pub fn split_evenly(data: &[u8], count: usize) -> Vec<Frame> {
    if count == 0 || data.is_empty() {
        return vec![Frame::new(data.to_vec())];
    }
    let chunk_size = data.len().div_ceil(count);
    let offsets = (1..count)
        .map(|index| (index * chunk_size).min(data.len()))
        .collect::<Vec<_>>();
    split_at(data, &offsets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_splits_can_bisect_utf8() {
        let data = "a💡z".as_bytes();
        let frames = split_at(data, &[2, 4]);
        let joined = frames
            .into_iter()
            .flat_map(|frame| frame.data)
            .collect::<Vec<_>>();
        assert_eq!(joined, data);
    }
}
