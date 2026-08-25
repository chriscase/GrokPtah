use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use super::isolated_visual_frames::{IsolatedVisualFrame, ISOLATED_VISUAL_FRAME_CHUNK_BYTES};
use super::isolated_visual_input::IsolatedVisualInputMessage;
use super::isolated_visual_input_wire::ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES;
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

pub const ISOLATED_VISUAL_STREAM_LENGTH_BYTES: usize = 4;
pub const ISOLATED_VISUAL_GUEST_INPUT_COMMAND: u8 = 4;
pub const ISOLATED_VISUAL_STREAM_MAX_FRAME_PACKET_BYTES: usize =
    100 + ISOLATED_VISUAL_FRAME_CHUNK_BYTES + 32;

/// Length-delimited transport seam for the private virtio socket.
///
/// The stream owns no socket policy and never exposes raw bytes to a model.
/// It bounds the allocation before reading, delegates authentication and
/// freshness to [`IsolatedVisualRuntimeSession`], and writes only packets
/// produced by the host input gate. A packaged supervisor still has to supply
/// the actual VSOCK connection and lifecycle authority.
pub struct IsolatedVisualStream<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> IsolatedVisualStream<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    pub fn into_parts(self) -> (R, W) {
        (self.reader, self.writer)
    }
}

impl<R: Read, W: Write> IsolatedVisualStream<R, W> {
    /// Reads one complete guest frame chunk. EOF is terminal; a zero or
    /// over-bound length fails closed before any allocation.
    pub fn read_frame_chunk(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
    ) -> ComputerResult<Option<IsolatedVisualFrame>> {
        let length = self.read_length(ISOLATED_VISUAL_STREAM_MAX_FRAME_PACKET_BYTES)?;
        let mut packet = vec![0_u8; length];
        read_exact(&mut self.reader, &mut packet)?;
        runtime.open_frame_chunk(&packet)
    }

    /// Seals and writes exactly one host input packet. The packet is length
    /// delimited for the private transport and authenticated before writing.
    pub fn write_input(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
    ) -> ComputerResult<()> {
        let packet = runtime.seal_input(input_sequence, request_nonce, message)?;
        if packet.len() > ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated input packet exceeds the stream bound",
            ));
        }
        self.writer
            .write_all(&[ISOLATED_VISUAL_GUEST_INPUT_COMMAND])
            .map_err(stream_error)?;
        self.writer
            .write_all(&(packet.len() as u32).to_be_bytes())
            .map_err(stream_error)?;
        self.writer.write_all(&packet).map_err(stream_error)?;
        self.writer.flush().map_err(stream_error)
    }

    fn read_length(&mut self, maximum: usize) -> ComputerResult<usize> {
        let mut length_bytes = [0_u8; ISOLATED_VISUAL_STREAM_LENGTH_BYTES];
        read_exact(&mut self.reader, &mut length_bytes)?;
        let length = u32::from_be_bytes(length_bytes) as usize;
        if length == 0 || length > maximum {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated stream packet length exceeds its bound",
            ));
        }
        Ok(length)
    }
}

impl<R: Read + AsRawFd, W: Write> IsolatedVisualStream<R, W> {
    /// Reads one complete guest frame chunk with a finite wait. The packaged
    /// supervisor uses this path so a silent guest cannot hold a run forever.
    pub fn read_frame_chunk_with_timeout(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
        timeout: Duration,
    ) -> ComputerResult<Option<IsolatedVisualFrame>> {
        let deadline = Instant::now() + timeout;
        let length = self
            .read_length_with_deadline(ISOLATED_VISUAL_STREAM_MAX_FRAME_PACKET_BYTES, deadline)?;
        let mut packet = vec![0_u8; length];
        read_exact_with_deadline(&mut self.reader, &mut packet, deadline)?;
        runtime.open_frame_chunk(&packet)
    }

    fn read_length_with_deadline(
        &mut self,
        maximum: usize,
        deadline: Instant,
    ) -> ComputerResult<usize> {
        let mut length_bytes = [0_u8; ISOLATED_VISUAL_STREAM_LENGTH_BYTES];
        read_exact_with_deadline(&mut self.reader, &mut length_bytes, deadline)?;
        let length = u32::from_be_bytes(length_bytes) as usize;
        if length == 0 || length > maximum {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated stream packet length exceeds its bound",
            ));
        }
        Ok(length)
    }
}

impl<R: Read, W: Write + AsRawFd> IsolatedVisualStream<R, W> {
    /// Seals and writes one host input packet with a finite writable deadline.
    /// The packaged supervisor uses a raw pipe here so a stalled guest cannot
    /// fill a buffered writer and hold the host indefinitely.
    pub fn write_input_with_timeout(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
        timeout: Duration,
    ) -> ComputerResult<()> {
        let packet = runtime.seal_input(input_sequence, request_nonce, message)?;
        if packet.len() > ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated input packet exceeds the stream bound",
            ));
        }
        let deadline = Instant::now() + timeout;
        write_all_with_deadline(
            &mut self.writer,
            &[ISOLATED_VISUAL_GUEST_INPUT_COMMAND],
            deadline,
        )?;
        write_all_with_deadline(
            &mut self.writer,
            &(packet.len() as u32).to_be_bytes(),
            deadline,
        )?;
        write_all_with_deadline(&mut self.writer, &packet, deadline)
    }
}

fn read_exact<R: Read>(reader: &mut R, bytes: &mut [u8]) -> ComputerResult<()> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            ComputerError::new(
                ComputerErrorCode::TargetClosed,
                "isolated visual stream closed mid-packet",
            )
        } else {
            stream_error(error)
        }
    })
}

fn stream_error(error: io::Error) -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::BackendFailure,
        format!("isolated visual stream I/O failed: {error}"),
    )
}

fn read_exact_with_deadline<R: Read + AsRawFd>(
    reader: &mut R,
    bytes: &mut [u8],
    deadline: Instant,
) -> ComputerResult<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_millis = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: the descriptor is borrowed from the supervisor-owned
        // reader and remains valid for this bounded poll/read iteration.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if ready == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated visual frame read timed out",
            ));
        }
        if ready < 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated visual frame poll failed",
            ));
        }
        match reader.read(&mut bytes[offset..]) {
            Ok(0) => {
                return Err(ComputerError::new(
                    ComputerErrorCode::TargetClosed,
                    "isolated visual stream closed mid-packet",
                ));
            }
            Ok(read) => offset += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(stream_error(error)),
        }
    }
    Ok(())
}

fn write_all_with_deadline<W: Write + AsRawFd>(
    writer: &mut W,
    bytes: &[u8],
    deadline: Instant,
) -> ComputerResult<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_millis = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: writer.as_raw_fd(),
            events: libc::POLLOUT | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: the descriptor is borrowed from the supervisor-owned
        // writer and remains valid for this bounded poll/write iteration.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if ready == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated visual input write timed out",
            ));
        }
        if ready < 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated visual input write poll failed",
            ));
        }
        match writer.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(ComputerError::new(
                    ComputerErrorCode::TargetClosed,
                    "isolated visual input pipe closed",
                ));
            }
            Ok(written) => offset += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(stream_error(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn rejects_zero_and_oversized_lengths_before_payload_read() {
        let mut stream = IsolatedVisualStream::new(Cursor::new(vec![0, 0, 0, 0]), Vec::new());
        let error = stream
            .read_length(ISOLATED_VISUAL_STREAM_MAX_FRAME_PACKET_BYTES)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::LimitReached);

        let mut stream =
            IsolatedVisualStream::new(Cursor::new((u32::MAX).to_be_bytes().to_vec()), Vec::new());
        let error = stream
            .read_length(ISOLATED_VISUAL_STREAM_MAX_FRAME_PACKET_BYTES)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::LimitReached);
    }

    #[test]
    fn maps_mid_packet_eof_to_target_closed() {
        let mut stream = IsolatedVisualStream::new(Cursor::new(vec![0, 0, 0, 4, 1]), Vec::new());
        assert_eq!(
            stream
                .read_length(ISOLATED_VISUAL_STREAM_MAX_FRAME_PACKET_BYTES)
                .unwrap(),
            4
        );
        let mut payload = [0_u8; 4];
        let error = read_exact(&mut stream.reader, &mut payload).unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::TargetClosed);
    }
}
