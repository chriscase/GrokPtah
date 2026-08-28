//! Adapter from the isolated-visual authority onto Computer Use errors.
//!
//! Isolated guest authority lives entirely in `grokptah-isolated-visual` and
//! binds to canonical Computer Run / WorkAttempt identifiers. This module maps
//! error taxonomies and nothing else; it deliberately does not keep a second
//! ledger, lease table, or dispatch de-duplication map.

use grokptah_isolated_visual::IsolatedErrorCode;

use super::types::{ComputerError, ComputerErrorCode};

pub use grokptah_isolated_visual::{
    AdmittedGuestImage, AdmittedHelperIdentity, ComputerSurfaceLease, DenyReason,
    IsolatedEvidenceClass, IsolatedPreflight, IsolatedVisualHost, IsolatedVisualProjection,
    PackagedTrustRoot,
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

/// Production admission, evaluated against this host right now.
///
/// It reads the configured artifact root and operator trust root and runs the
/// OS code-signing probe. On a host without those it denies with reasons; it
/// never enables an unsupported environment and never returns a value that
/// reads as eligible by default.
pub fn isolated_visual_admission() -> IsolatedPreflight {
    IsolatedPreflight::inspect_production()
}
