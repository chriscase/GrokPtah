use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{sha256_hex, validate_digest, validate_id, SCHEMA_VERSION};
use crate::manifest::ComputerSurfaceBinding;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperExitObservation {
    pub guest_id: String,
    pub surface_incarnation: String,
    pub helper_alive: bool,
    pub audit_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmStopObservation {
    pub guest_id: String,
    pub surface_incarnation: String,
    pub vm_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OverlayRemovalObservation {
    pub guest_id: String,
    pub surface_incarnation: String,
    pub overlay_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelRevocationObservation {
    pub guest_id: String,
    pub surface_incarnation: String,
    pub channel_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OccupancyReleaseObservation {
    pub guest_id: String,
    pub surface_incarnation: String,
    pub occupancy_held: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedCleanupObservation {
    pub guest_id: String,
    pub surface: ComputerSurfaceBinding,
    pub helper_exit: Option<HelperExitObservation>,
    pub vm_stop: Option<VmStopObservation>,
    pub overlay_removed: Option<OverlayRemovalObservation>,
    pub channel_revoked: Option<ChannelRevocationObservation>,
    pub occupancy_released: Option<OccupancyReleaseObservation>,
    pub resident_bytes: Option<u64>,
}

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
    pub occupancy_released: bool,
    pub resident_bytes: u64,
    pub helper_exit_digest: String,
    pub vm_stop_digest: String,
    pub overlay_digest: String,
    pub channel_digest: String,
    pub occupancy_digest: String,
    pub verified_at: DateTime<Utc>,
}

impl IsolatedCleanupEvidence {
    pub fn from_observations(
        observation: IsolatedCleanupObservation,
        now: DateTime<Utc>,
    ) -> IsolatedResult<Self> {
        if observation.guest_id != observation.surface.surface_id
            && observation.surface.incarnation.is_empty()
        {
            return Err(IsolatedError::unauthorized(
                "cleanup observation is missing a surface incarnation",
            ));
        }
        let helper = observation
            .helper_exit
            .as_ref()
            .ok_or_else(|| IsolatedError::uncertain("helper exit was not observed"))?;
        let vm = observation
            .vm_stop
            .as_ref()
            .ok_or_else(|| IsolatedError::uncertain("VM stop was not observed"))?;
        let overlay = observation
            .overlay_removed
            .as_ref()
            .ok_or_else(|| IsolatedError::uncertain("overlay removal was not observed"))?;
        let channel = observation
            .channel_revoked
            .as_ref()
            .ok_or_else(|| IsolatedError::uncertain("channel revocation was not observed"))?;
        let occupancy = observation
            .occupancy_released
            .as_ref()
            .ok_or_else(|| IsolatedError::uncertain("occupancy release was not observed"))?;
        let resident_bytes = observation
            .resident_bytes
            .ok_or_else(|| IsolatedError::uncertain("resident-frame bytes were not observed"))?;
        bind(
            helper.guest_id.as_str(),
            helper.surface_incarnation.as_str(),
            &observation,
        )?;
        bind(
            vm.guest_id.as_str(),
            vm.surface_incarnation.as_str(),
            &observation,
        )?;
        bind(
            overlay.guest_id.as_str(),
            overlay.surface_incarnation.as_str(),
            &observation,
        )?;
        bind(
            channel.guest_id.as_str(),
            channel.surface_incarnation.as_str(),
            &observation,
        )?;
        bind(
            occupancy.guest_id.as_str(),
            occupancy.surface_incarnation.as_str(),
            &observation,
        )?;
        let evidence = Self {
            schema_version: SCHEMA_VERSION,
            guest_id: observation.guest_id,
            surface: observation.surface,
            helper_exited: !helper.helper_alive,
            guest_destroyed: !vm.vm_present,
            overlay_removed: !overlay.overlay_present,
            channel_revoked: !channel.channel_present,
            occupancy_released: !occupancy.occupancy_held,
            resident_bytes,
            helper_exit_digest: digest_of(helper),
            vm_stop_digest: digest_of(vm),
            overlay_digest: digest_of(overlay),
            channel_digest: digest_of(channel),
            occupancy_digest: digest_of(occupancy),
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
        validate_digest("helper_exit_digest", &self.helper_exit_digest)?;
        validate_digest("vm_stop_digest", &self.vm_stop_digest)?;
        validate_digest("overlay_digest", &self.overlay_digest)?;
        validate_digest("channel_digest", &self.channel_digest)?;
        validate_digest("occupancy_digest", &self.occupancy_digest)?;
        if !self.helper_exited
            || !self.guest_destroyed
            || !self.overlay_removed
            || !self.channel_revoked
            || !self.occupancy_released
            || self.resident_bytes != 0
        {
            return Err(IsolatedError::uncertain(
                "isolated guest cleanup evidence is incomplete",
            ));
        }
        Ok(())
    }
}

fn bind(
    guest_id: &str,
    incarnation: &str,
    observation: &IsolatedCleanupObservation,
) -> IsolatedResult<()> {
    if guest_id != observation.guest_id || incarnation != observation.surface.incarnation {
        return Err(IsolatedError::unauthorized(
            "cleanup observation is not bound to the guest surface incarnation",
        ));
    }
    Ok(())
}

fn digest_of<T: Serialize>(value: &T) -> String {
    sha256_hex(&serde_json::to_vec(value).unwrap_or_default())
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
    use crate::manifest::ComputerSurfaceBinding;

    fn observation(incarnation: &str) -> IsolatedCleanupObservation {
        let surface = ComputerSurfaceBinding {
            surface_id: "surface-1".into(),
            incarnation: incarnation.into(),
        };
        IsolatedCleanupObservation {
            guest_id: "guest-1".into(),
            surface: surface.clone(),
            helper_exit: Some(HelperExitObservation {
                guest_id: "guest-1".into(),
                surface_incarnation: incarnation.into(),
                helper_alive: false,
                audit_identity: "helper-audit-1".into(),
            }),
            vm_stop: Some(VmStopObservation {
                guest_id: "guest-1".into(),
                surface_incarnation: incarnation.into(),
                vm_present: false,
            }),
            overlay_removed: Some(OverlayRemovalObservation {
                guest_id: "guest-1".into(),
                surface_incarnation: incarnation.into(),
                overlay_present: false,
            }),
            channel_revoked: Some(ChannelRevocationObservation {
                guest_id: "guest-1".into(),
                surface_incarnation: incarnation.into(),
                channel_present: false,
            }),
            occupancy_released: Some(OccupancyReleaseObservation {
                guest_id: "guest-1".into(),
                surface_incarnation: incarnation.into(),
                occupancy_held: false,
            }),
            resident_bytes: Some(0),
        }
    }

    #[test]
    fn fabricated_booleans_cannot_mark_cleaned() {
        let surface = ComputerSurfaceBinding::issue();
        let fake = IsolatedCleanupEvidence {
            schema_version: SCHEMA_VERSION,
            guest_id: "guest-1".into(),
            surface,
            helper_exited: true,
            guest_destroyed: true,
            overlay_removed: true,
            channel_revoked: true,
            occupancy_released: true,
            resident_bytes: 0,
            helper_exit_digest: "short".into(),
            vm_stop_digest: "short".into(),
            overlay_digest: "short".into(),
            channel_digest: "short".into(),
            occupancy_digest: "short".into(),
            verified_at: Utc::now(),
        };
        assert!(fake.validate().is_err());
    }

    #[test]
    fn incomplete_observation_is_uncertain() {
        let mut obs = observation("incarnation-1");
        obs.overlay_removed.as_mut().unwrap().overlay_present = true;
        assert_eq!(
            IsolatedCleanupEvidence::from_observations(obs, Utc::now())
                .unwrap_err()
                .code,
            crate::error::IsolatedErrorCode::UncertainOutcome
        );
    }

    #[test]
    fn old_incarnation_cannot_satisfy_cleanup() {
        let mut obs = observation("incarnation-1");
        obs.helper_exit.as_mut().unwrap().surface_incarnation = "old-incarnation".into();
        assert_eq!(
            IsolatedCleanupEvidence::from_observations(obs, Utc::now())
                .unwrap_err()
                .code,
            crate::error::IsolatedErrorCode::Unauthorized
        );
    }

    #[test]
    fn complete_observations_validate() {
        IsolatedCleanupEvidence::from_observations(observation("incarnation-1"), Utc::now())
            .unwrap();
    }
}
