//! Non-macOS builds expose the type for API stability; native observation is macOS-only.

use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

#[derive(Debug)]
pub struct MacOsObservationPlatform;

impl MacOsObservationPlatform {
    pub fn new_native() -> ComputerResult<Self> {
        Err(ComputerError::new(
            ComputerErrorCode::UnsupportedPlatform,
            "macOS computer-use observation is not available on this platform",
        ))
    }
}
