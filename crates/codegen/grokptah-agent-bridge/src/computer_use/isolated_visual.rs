//! Thin adapter from the isolated-visual crate onto Computer Use errors.
//!
//! Isolated guest authority lives in `grokptah-isolated-visual` and binds to
//! canonical Computer Run / WorkAttempt identifiers. This module does not
//! create a second ledger.

use grokptah_isolated_visual::IsolatedErrorCode;

use super::types::{ComputerError, ComputerErrorCode};

pub use grokptah_isolated_visual::{
    ComputerSurfaceLease, IsolatedEvidenceClass, IsolatedPreflight, IsolatedVisualHost,
    IsolatedVisualProjection,
};

pub fn map_isolated_error(error: grokptah_isolated_visual::IsolatedError) -> ComputerError {
    let code = match error.code {
        IsolatedErrorCode::InvalidRequest => ComputerErrorCode::InvalidRequest,
        IsolatedErrorCode::InvalidState => ComputerErrorCode::InvalidState,
        IsolatedErrorCode::Unauthorized => ComputerErrorCode::Unauthorized,
        IsolatedErrorCode::PermissionRequired => ComputerErrorCode::PermissionRequired,
        IsolatedErrorCode::PermissionDenied => ComputerErrorCode::PermissionDenied,
        IsolatedErrorCode::ForbiddenTarget => ComputerErrorCode::ForbiddenTarget,
        IsolatedErrorCode::ForbiddenAction => ComputerErrorCode::ForbiddenAction,
        IsolatedErrorCode::StaleObservation => ComputerErrorCode::StaleObservation,
        IsolatedErrorCode::TargetChanged => ComputerErrorCode::TargetChanged,
        IsolatedErrorCode::LimitReached => ComputerErrorCode::LimitReached,
        IsolatedErrorCode::Conflict => ComputerErrorCode::Conflict,
        IsolatedErrorCode::Pending => ComputerErrorCode::Pending,
        IsolatedErrorCode::UncertainOutcome => ComputerErrorCode::UncertainOutcome,
        IsolatedErrorCode::Interrupted => ComputerErrorCode::Interrupted,
        IsolatedErrorCode::BackendUnavailable => ComputerErrorCode::BackendUnavailable,
        IsolatedErrorCode::BackendFailure => ComputerErrorCode::BackendFailure,
        IsolatedErrorCode::UnsupportedPlatform => ComputerErrorCode::UnsupportedPlatform,
        IsolatedErrorCode::Internal => ComputerErrorCode::Internal,
    };
    ComputerError::new(code, error.message)
}

/// Isolated visual is unavailable until Virtualization.framework preflight
/// and signed artifacts pass. Simulator compilation is not admission.
pub fn isolated_visual_admission() -> IsolatedPreflight {
    IsolatedPreflight::inspect(None).unwrap_or_else(|_| IsolatedPreflight {
        hardware_supported: false,
        virtualization_framework_present: false,
        signed_helper_present: false,
        signed_image_present: false,
        free_bytes: 0,
        occupancy_clear: false,
        allowed_to_launch: false,
        deny_reason: Some("isolated visual preflight failed closed".into()),
        evidence_class_if_launched: IsolatedEvidenceClass::SimulatorIneligible,
    })
}
