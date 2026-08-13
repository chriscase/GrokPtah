//! Byte-safe, bounded buffering for compatible-provider responses.

use anyhow::{bail, Result};

pub const DEFAULT_MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;

fn decode_strict(bytes: Vec<u8>, what: &str) -> Result<String> {
    String::from_utf8(bytes).map_err(|error| {
        anyhow::anyhow!("{what} was not valid UTF-8; refusing lossy decoding: {error}")
    })
}

pub struct SseLineDecoder {
    pending: Vec<u8>,
    max_line_bytes: usize,
}

impl SseLineDecoder {
    pub fn new() -> Self {
        Self::with_max_line_bytes(DEFAULT_MAX_BUFFERED_BYTES)
    }

    pub fn with_max_line_bytes(max_line_bytes: usize) -> Self {
        Self {
            pending: Vec::new(),
            max_line_bytes,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            if position > self.max_line_bytes {
                bail!("provider SSE line exceeded {} bytes", self.max_line_bytes);
            }
            let mut line: Vec<u8> = self.pending.drain(..=position).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(decode_strict(line, "provider SSE line")?);
        }
        if self.pending.len() > self.max_line_bytes {
            bail!(
                "provider SSE line exceeded {} bytes without a terminator",
                self.max_line_bytes
            );
        }
        Ok(lines)
    }

    pub fn finish(self) -> Result<Option<String>> {
        if self.pending.is_empty() {
            Ok(None)
        } else {
            decode_strict(self.pending, "trailing provider SSE data").map(Some)
        }
    }
}

impl Default for SseLineDecoder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct BoundedBodyAccumulator {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl BoundedBodyAccumulator {
    pub fn new() -> Self {
        Self::with_max_bytes(DEFAULT_MAX_BUFFERED_BYTES)
    }

    pub fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if self.bytes.len().saturating_add(bytes.len()) > self.max_bytes {
            bail!("provider response exceeded {} bytes", self.max_bytes);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub fn finish(self) -> Result<String> {
        decode_strict(self.bytes, "provider response body")
    }
}

impl Default for BoundedBodyAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_multibyte_utf8_is_preserved() {
        let line = "data: café 日本語 🚀\n";
        let mut decoder = SseLineDecoder::new();
        let mut output = Vec::new();
        for byte in line.as_bytes() {
            output.extend(decoder.push(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(output, vec!["data: café 日本語 🚀"]);
    }

    #[test]
    fn crlf_and_unterminated_final_line_are_supported() {
        let mut decoder = SseLineDecoder::new();
        assert_eq!(
            decoder.push(b"data: one\r\ndata: two").unwrap(),
            vec!["data: one"]
        );
        assert_eq!(decoder.finish().unwrap(), Some("data: two".into()));
    }

    #[test]
    fn complete_and_unterminated_oversized_lines_fail_closed() {
        for terminator in [false, true] {
            let mut decoder = SseLineDecoder::with_max_line_bytes(8);
            let mut bytes = vec![b'x'; 9];
            if terminator {
                bytes.push(b'\n');
            }
            assert!(decoder
                .push(&bytes)
                .unwrap_err()
                .to_string()
                .contains("exceeded"));
        }
    }

    #[test]
    fn bounded_body_rejects_growth_before_copying() {
        let mut body = BoundedBodyAccumulator::with_max_bytes(4);
        body.push(b"1234").unwrap();
        assert!(body.push(b"5").is_err());
    }
}
