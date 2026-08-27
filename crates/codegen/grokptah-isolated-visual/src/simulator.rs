use std::collections::BTreeMap;

use crate::error::{IsolatedError, IsolatedResult};
use crate::protocol::{IsolatedFrameMeta, IsolatedInputEvent, ResidentFrame};

/// Deterministic in-process guest. Never injects into the host window server,
/// clipboard, or pointer. Evidence from this backend is ineligible for VM
/// qualification.
#[derive(Debug, Default)]
pub struct IsolatedSimulator {
    frames: BTreeMap<String, Option<ResidentFrame>>,
    inputs: BTreeMap<String, Vec<IsolatedInputEvent>>,
}

impl IsolatedSimulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attach(&mut self, guest_id: &str) {
        self.frames.entry(guest_id.to_string()).or_insert(None);
        self.inputs.entry(guest_id.to_string()).or_default();
    }

    pub fn ingest_frame(
        &mut self,
        guest_id: &str,
        frame: ResidentFrame,
        max_resident: u64,
    ) -> IsolatedResult<u64> {
        self.attach(guest_id);
        if frame.byte_len() > max_resident {
            return Err(IsolatedError::limit("frame exceeds resident budget"));
        }
        let previous = self.frames.insert(guest_id.to_string(), Some(frame));
        let _ = previous;
        Ok(self.resident_bytes(guest_id))
    }

    pub fn rotate_out(&mut self, guest_id: &str) -> u64 {
        let previous = self.frames.insert(guest_id.to_string(), None);
        previous
            .flatten()
            .map(|frame| frame.byte_len())
            .unwrap_or(0)
    }

    pub fn accept_input(&mut self, event: IsolatedInputEvent) -> IsolatedResult<()> {
        self.attach(&event.guest_id);
        self.inputs
            .entry(event.guest_id.clone())
            .or_default()
            .push(event);
        Ok(())
    }

    pub fn resident_bytes(&self, guest_id: &str) -> u64 {
        self.frames
            .get(guest_id)
            .and_then(|frame| frame.as_ref())
            .map(ResidentFrame::byte_len)
            .unwrap_or(0)
    }

    pub fn latest_meta(&self, guest_id: &str) -> Option<&IsolatedFrameMeta> {
        self.frames
            .get(guest_id)
            .and_then(|frame| frame.as_ref())
            .map(|frame| &frame.meta)
    }

    pub fn destroy(&mut self, guest_id: &str) {
        self.frames.remove(guest_id);
        self.inputs.remove(guest_id);
    }

    pub fn input_len(&self, guest_id: &str) -> usize {
        self.inputs.get(guest_id).map(Vec::len).unwrap_or(0)
    }
}
