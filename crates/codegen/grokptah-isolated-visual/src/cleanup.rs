use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{validate_id, SCHEMA_VERSION};
use crate::manifest::ComputerSurfaceBinding;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedCleanupEvidence {
    pub schema_version: u32,
    pub guest_id: String,
    pub surface: ComputerSurfaceBinding,
    pub helper_exited: bool,
    pub guest_destroyed: bool,
    pub overlay_removed: bool,
    pub channel_revoked: bool,
    pub resident_bytes: u64,
    pub verified_at: DateTime<Utc>,
}

impl IsolatedCleanupEvidence {
    pub fn verified(
        guest_id: impl Into<String>,
        surface: ComputerSurfaceBinding,
        now: DateTime<Utc>,
    ) -> IsolatedResult<Self> {
        let evidence = Self {
            schema_version: SCHEMA_VERSION,
            guest_id: guest_id.into(),
            surface,
            helper_exited: true,
            guest_destroyed: true,
            overlay_removed: true,
            channel_revoked: true,
            resident_bytes: 0,
            verified_at: now,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(IsolatedError::internal(
                "cleanup evidence schema is unsupported",
            ));
        }
        validate_id("guest_id", &self.guest_id)?;
        self.surface.validate()?;
        if !self.helper_exited
            || !self.guest_destroyed
            || !self.overlay_removed
            || !self.channel_revoked
            || self.resident_bytes != 0
        {
            return Err(IsolatedError::uncertain(
                "isolated guest cleanup evidence is incomplete",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedCleanupReason {
    Success,
    Cancel,
    Timeout,
    HelperFailure,
    GuestCrash,
    HostCrash,
    Disconnect,
    Restart,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_cleanup_is_uncertain() {
        let mut evidence = IsolatedCleanupEvidence {
            schema_version: SCHEMA_VERSION,
            guest_id: "guest-1".into(),
            surface: ComputerSurfaceBinding::issue(),
            helper_exited: true,
            guest_destroyed: false,
            overlay_removed: true,
            channel_revoked: true,
            resident_bytes: 0,
            verified_at: Utc::now(),
        };
        assert!(evidence.validate().is_err());
        evidence.guest_destroyed = true;
        evidence.validate().unwrap();
    }
}
