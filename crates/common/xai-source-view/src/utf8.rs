//! Incremental UTF-8 decoding across chunk boundaries.
//!
//! A bounded read can end in the middle of a multi-byte character. Decoding
//! each chunk independently would either drop that character or emit a
//! replacement for a sequence that is in fact perfectly valid — the classic
//! way a paged viewer corrupts every emoji that happens to straddle a
//! boundary. This decoder carries the incomplete tail forward instead.

/// Streaming UTF-8 decoder with a carry of at most three bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Utf8Decoder {
    carry: Vec<u8>,
}

/// One chunk's decode result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedChunk {
    pub text: String,
    /// How many invalid sequences became U+FFFD.
    pub replacements: usize,
}

impl Utf8Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resume with a carry recovered from a cursor.
    ///
    /// A carry longer than three bytes cannot come from this decoder, so it is
    /// rejected rather than trusted.
    pub fn resume(carry: Vec<u8>) -> Option<Self> {
        if carry.len() > 3 {
            return None;
        }
        Some(Self { carry })
    }

    /// Bytes held back from the previous chunk.
    pub fn carry(&self) -> &[u8] {
        &self.carry
    }

    pub fn has_carry(&self) -> bool {
        !self.carry.is_empty()
    }

    /// Decode one chunk.
    ///
    /// With `at_eof` false an incomplete trailing sequence is carried forward;
    /// with `at_eof` true it is emitted as a replacement character, because
    /// there is nothing left to complete it.
    pub fn decode(&mut self, chunk: &[u8], at_eof: bool) -> DecodedChunk {
        let mut buffer = Vec::with_capacity(self.carry.len() + chunk.len());
        buffer.extend_from_slice(&self.carry);
        buffer.extend_from_slice(chunk);
        self.carry.clear();

        let mut text = String::with_capacity(buffer.len());
        let mut replacements = 0usize;
        let mut rest: &[u8] = &buffer;

        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    // SAFETY-FREE: `valid_up_to` is guaranteed to be a
                    // character boundary of valid UTF-8 by the error contract.
                    text.push_str(std::str::from_utf8(&rest[..valid_up_to]).unwrap_or_default());
                    match error.error_len() {
                        Some(length) => {
                            text.push('\u{FFFD}');
                            replacements += 1;
                            rest = &rest[valid_up_to + length..];
                        }
                        None => {
                            // Truncated but so far valid: carry it, unless the
                            // stream has ended.
                            let tail = &rest[valid_up_to..];
                            if at_eof {
                                text.push('\u{FFFD}');
                                replacements += 1;
                            } else {
                                self.carry.extend_from_slice(tail);
                            }
                            break;
                        }
                    }
                }
            }
        }

        DecodedChunk { text, replacements }
    }
}
