//! Durable provider-quota reservation records.
//!
//! Reservations are authoritative ledger rows. Pool usage is derived from
//! those rows under the orchestration store lock; there is deliberately no
//! separately mutable "remaining" counter that could drift after a crash.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use super::types::{hash_payload, OrchError, OrchErrorCode, RunRecord};

pub const QUOTA_LEDGER_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_QUOTA_WINDOW_MS: u64 = 60 * 60 * 1_000;
pub const DEFAULT_MAX_IN_FLIGHT_RESERVATIONS: u32 = 4;
pub const DEFAULT_MAX_TOKENS_PER_WINDOW: u64 = 10_000_000;
pub const DEFAULT_MAX_REQUESTS_PER_WINDOW: u64 = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaClass {
    CodingExecution,
    ManagerProposal,
    Qualification,
    ComputerSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaPoolKey {
    /// Legacy owner captured on old ledger rows. Capacity identity ignores it;
    /// [`QuotaReservation::owner_id`] is the auth/audit principal.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_id: String,
    pub workspace: String,
    pub provider_id: String,
    pub credential_fingerprint: String,
    pub class: QuotaClass,
}

impl QuotaPoolKey {
    pub fn pool_id(&self) -> String {
        hash_payload(&serde_json::json!({
            "workspace": self.workspace,
            "providerId": self.provider_id,
            "credentialFingerprint": self.credential_fingerprint,
            "class": self.class,
        }))
    }

    pub fn same_capacity_pool(&self, other: &Self) -> bool {
        self.workspace == other.workspace
            && self.provider_id == other.provider_id
            && self.credential_fingerprint == other.credential_fingerprint
            && self.class == other.class
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        for (value, field) in [
            (self.workspace.as_str(), "workspace"),
            (self.provider_id.as_str(), "provider_id"),
            (
                self.credential_fingerprint.as_str(),
                "credential_fingerprint",
            ),
        ] {
            if value.is_empty() || value.len() > 2_048 || value.contains('\0') {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("quota pool {field} is invalid"),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaLimits {
    pub window_ms: u64,
    pub max_in_flight_reservations: u32,
    pub max_tokens_per_window: u64,
    pub max_requests_per_window: u64,
}

impl Default for QuotaLimits {
    fn default() -> Self {
        Self {
            window_ms: DEFAULT_QUOTA_WINDOW_MS,
            max_in_flight_reservations: DEFAULT_MAX_IN_FLIGHT_RESERVATIONS,
            max_tokens_per_window: DEFAULT_MAX_TOKENS_PER_WINDOW,
            max_requests_per_window: DEFAULT_MAX_REQUESTS_PER_WINDOW,
        }
    }
}

impl QuotaLimits {
    pub fn validate(self) -> Result<(), OrchError> {
        if self.window_ms == 0
            || self.max_in_flight_reservations == 0
            || self.max_tokens_per_window == 0
            || self.max_requests_per_window == 0
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "quota limits must all be greater than zero",
            ));
        }
        if self.window_ms > i64::MAX as u64 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "quota window is too large",
            ));
        }
        Ok(())
    }

    pub fn window_started_at(self, now: DateTime<Utc>) -> Result<DateTime<Utc>, OrchError> {
        self.validate()?;
        let now_ms = now.timestamp_millis();
        let window_ms = i64::try_from(self.window_ms).map_err(|_| {
            OrchError::new(OrchErrorCode::InvalidRequest, "quota window is too large")
        })?;
        let start_ms = now_ms
            .div_euclid(window_ms)
            .checked_mul(window_ms)
            .ok_or_else(|| {
                OrchError::new(OrchErrorCode::InvalidRequest, "quota window overflowed")
            })?;
        Utc.timestamp_millis_opt(start_ms).single().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "quota window timestamp is invalid",
            )
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaReservationState {
    Reserved,
    Consumed,
    Refunded,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaReservation {
    pub schema_version: u32,
    pub reservation_id: String,
    /// Auth/audit principal that admitted this reservation. Not part of pool
    /// capacity identity, so desktop (`primary`) and native service owners
    /// share one host-wide slot budget.
    #[serde(default)]
    pub owner_id: String,
    pub pool: QuotaPoolKey,
    pub pool_id: String,
    pub run_id: String,
    pub route_snapshot_hash: String,
    pub limits: QuotaLimits,
    pub window_started_at: DateTime<Utc>,
    pub tokens_reserved: u64,
    pub requests_reserved: u64,
    pub tokens_consumed: u64,
    pub requests_consumed: u64,
    pub state: QuotaReservationState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for QuotaReservation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            schema_version: u32,
            reservation_id: String,
            #[serde(default)]
            owner_id: String,
            pool: QuotaPoolKey,
            pool_id: String,
            run_id: String,
            route_snapshot_hash: String,
            limits: QuotaLimits,
            window_started_at: DateTime<Utc>,
            tokens_reserved: u64,
            requests_reserved: u64,
            tokens_consumed: u64,
            requests_consumed: u64,
            state: QuotaReservationState,
            created_at: DateTime<Utc>,
            updated_at: DateTime<Utc>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let mut reservation = Self {
            schema_version: wire.schema_version,
            reservation_id: wire.reservation_id,
            owner_id: wire.owner_id,
            pool: wire.pool,
            pool_id: wire.pool_id,
            run_id: wire.run_id,
            route_snapshot_hash: wire.route_snapshot_hash,
            limits: wire.limits,
            window_started_at: wire.window_started_at,
            tokens_reserved: wire.tokens_reserved,
            requests_reserved: wire.requests_reserved,
            tokens_consumed: wire.tokens_consumed,
            requests_consumed: wire.requests_consumed,
            state: wire.state,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        };
        reservation.migrate_host_wide_pool();
        Ok(reservation)
    }
}

impl QuotaReservation {
    pub(crate) fn request_ceiling_for_run(run: &RunRecord) -> u64 {
        // One bounded recovery-grace model step may follow the configured
        // round budget. Compaction, planning, and child calls share this same
        // ceiling rather than silently creating unmetered provider requests.
        u64::from(run.bounds.max_rounds).saturating_add(1)
    }

    pub fn for_run(
        run: &RunRecord,
        owner_id: impl Into<String>,
        limits: QuotaLimits,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        let route = run.provider_route.as_ref().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "quota reservation requires a provider route snapshot",
            )
        })?;
        route.validate()?;
        let class = route.quota_class.ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider route has no quota class",
            )
        })?;
        let reservation_id = route.quota_reservation_id.clone().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider route has no quota reservation identity",
            )
        })?;
        let tokens_reserved = run.bounds.max_total_tokens.ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider-backed Run requires a finite token ceiling",
            )
        })?;
        limits.validate()?;
        let owner_id = owner_id.into();
        if owner_id.is_empty() || owner_id.len() > 2_048 || owner_id.contains('\0') {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "quota reservation owner_id is invalid",
            ));
        }
        let pool = QuotaPoolKey {
            owner_id: String::new(),
            workspace: run.workspace.clone(),
            provider_id: route.provider_id.clone(),
            credential_fingerprint: route.credential_fingerprint.clone(),
            class,
        };
        pool.validate()?;
        let reservation = Self {
            schema_version: QUOTA_LEDGER_SCHEMA_VERSION,
            reservation_id,
            owner_id,
            pool_id: pool.pool_id(),
            pool,
            run_id: run.run_id.clone(),
            route_snapshot_hash: route.snapshot_hash.clone(),
            limits,
            window_started_at: limits.window_started_at(now)?,
            tokens_reserved,
            requests_reserved: Self::request_ceiling_for_run(run),
            tokens_consumed: 0,
            requests_consumed: 0,
            state: QuotaReservationState::Reserved,
            created_at: now,
            updated_at: now,
        };
        reservation.validate()?;
        Ok(reservation)
    }

    /// Lift a legacy `pool.ownerId` onto the reservation and recompute the
    /// host-wide pool identity so restart does not split desktop/native slots.
    pub fn migrate_host_wide_pool(&mut self) {
        if self.owner_id.is_empty() {
            self.owner_id = std::mem::take(&mut self.pool.owner_id);
        } else if !self.pool.owner_id.is_empty() {
            self.pool.owner_id.clear();
        }
        self.pool_id = self.pool.pool_id();
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != QUOTA_LEDGER_SCHEMA_VERSION {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "quota reservation schema is invalid",
            ));
        }
        self.pool.validate()?;
        self.limits.validate()?;
        for (value, field) in [
            (self.owner_id.as_str(), "owner_id"),
            (self.reservation_id.as_str(), "reservation_id"),
            (self.pool_id.as_str(), "pool_id"),
            (self.run_id.as_str(), "run_id"),
            (self.route_snapshot_hash.as_str(), "route_snapshot_hash"),
        ] {
            if value.is_empty() || value.len() > 2_048 || value.contains('\0') {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("quota reservation {field} is invalid"),
                ));
            }
        }
        if self.pool_id != self.pool.pool_id() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "quota reservation pool identity does not match its immutable key",
            ));
        }
        if self.window_started_at != self.limits.window_started_at(self.created_at)? {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "quota reservation window does not match its creation time",
            ));
        }
        if self.tokens_reserved == 0 || self.requests_reserved == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "quota reservation amounts must be greater than zero",
            ));
        }
        if self.tokens_consumed > self.tokens_reserved
            || self.requests_consumed > self.requests_reserved
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "quota consumption exceeds its reservation",
            ));
        }
        if matches!(
            self.state,
            QuotaReservationState::Refunded | QuotaReservationState::Expired
        ) && (self.tokens_consumed != 0 || self.requests_consumed != 0)
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "released quota reservation retains consumption",
            ));
        }
        if self.updated_at < self.created_at {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "quota reservation was updated before it was created",
            ));
        }
        Ok(())
    }

    pub fn settle(
        &mut self,
        tokens_consumed: u64,
        requests_consumed: u64,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        if requests_consumed > self.requests_reserved {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "authoritative provider usage exceeds its quota reservation",
            ));
        }
        let tokens_consumed = tokens_consumed
            .min(self.tokens_reserved)
            .max(self.tokens_consumed);
        let requests_consumed = requests_consumed.max(self.requests_consumed);
        let state = if tokens_consumed == 0 && requests_consumed == 0 {
            QuotaReservationState::Refunded
        } else {
            QuotaReservationState::Consumed
        };
        if self.state == QuotaReservationState::Refunded {
            if state == QuotaReservationState::Refunded {
                return Ok(());
            }
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "released quota reservation cannot be consumed",
            ));
        }
        if self.state == QuotaReservationState::Expired {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "released quota reservation cannot be consumed",
            ));
        }
        self.tokens_consumed = tokens_consumed;
        self.requests_consumed = requests_consumed;
        self.state = state;
        self.updated_at = self.updated_at.max(now);
        self.validate()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaPoolUsage {
    pub in_flight_reservations: u32,
    pub tokens_committed: u64,
    pub requests_committed: u64,
}

impl QuotaPoolUsage {
    pub fn include(
        &mut self,
        reservation: &QuotaReservation,
        pool: &QuotaPoolKey,
        window_started_at: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        reservation.validate()?;
        if !reservation.pool.same_capacity_pool(pool) {
            return Ok(());
        }
        match reservation.state {
            QuotaReservationState::Reserved => {
                self.in_flight_reservations = self
                    .in_flight_reservations
                    .checked_add(1)
                    .ok_or_else(quota_overflow)?;
                if reservation.window_started_at == window_started_at {
                    self.tokens_committed = self
                        .tokens_committed
                        .checked_add(reservation.tokens_reserved)
                        .ok_or_else(quota_overflow)?;
                    self.requests_committed = self
                        .requests_committed
                        .checked_add(reservation.requests_reserved)
                        .ok_or_else(quota_overflow)?;
                }
            }
            QuotaReservationState::Consumed
                if reservation.window_started_at == window_started_at =>
            {
                self.tokens_committed = self
                    .tokens_committed
                    .checked_add(reservation.tokens_consumed)
                    .ok_or_else(quota_overflow)?;
                self.requests_committed = self
                    .requests_committed
                    .checked_add(reservation.requests_consumed)
                    .ok_or_else(quota_overflow)?;
            }
            QuotaReservationState::Consumed
            | QuotaReservationState::Refunded
            | QuotaReservationState::Expired => {}
        }
        Ok(())
    }

    pub fn ensure_can_reserve(self, reservation: &QuotaReservation) -> Result<(), OrchError> {
        let in_flight = self
            .in_flight_reservations
            .checked_add(1)
            .ok_or_else(quota_overflow)?;
        let tokens = self
            .tokens_committed
            .checked_add(reservation.tokens_reserved)
            .ok_or_else(quota_overflow)?;
        let requests = self
            .requests_committed
            .checked_add(reservation.requests_reserved)
            .ok_or_else(quota_overflow)?;
        if in_flight > reservation.limits.max_in_flight_reservations
            || tokens > reservation.limits.max_tokens_per_window
            || requests > reservation.limits.max_requests_per_window
        {
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                "provider quota capacity is exhausted",
            ));
        }
        Ok(())
    }
}

fn quota_overflow() -> OrchError {
    OrchError::new(
        OrchErrorCode::CapacityExhausted,
        "provider quota accounting overflowed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation(state: QuotaReservationState) -> QuotaReservation {
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 34, 56).unwrap();
        QuotaReservation {
            schema_version: QUOTA_LEDGER_SCHEMA_VERSION,
            reservation_id: "quota-run-1".into(),
            owner_id: "owner".into(),
            pool: QuotaPoolKey {
                owner_id: String::new(),
                workspace: "/workspace".into(),
                provider_id: "xai".into(),
                credential_fingerprint: "cred-fingerprint".into(),
                class: QuotaClass::CodingExecution,
            },
            pool_id: String::new(),
            run_id: "run-1".into(),
            route_snapshot_hash: "route-hash".into(),
            limits: QuotaLimits::default(),
            window_started_at: QuotaLimits::default().window_started_at(now).unwrap(),
            tokens_reserved: 1_000,
            requests_reserved: 10,
            tokens_consumed: 0,
            requests_consumed: 0,
            state,
            created_at: now,
            updated_at: now,
        }
        .with_pool_id()
    }

    impl QuotaReservation {
        fn with_pool_id(mut self) -> Self {
            self.pool_id = self.pool.pool_id();
            self
        }
    }

    #[test]
    fn deterministic_windows_and_checked_usage() {
        let first = reservation(QuotaReservationState::Reserved);
        let one_ms_later = first.created_at + chrono::Duration::milliseconds(1);
        assert_eq!(
            first.window_started_at,
            first.limits.window_started_at(one_ms_later).unwrap()
        );

        let mut usage = QuotaPoolUsage::default();
        usage
            .include(&first, &first.pool, first.window_started_at)
            .unwrap();
        assert_eq!(usage.in_flight_reservations, 1);
        assert_eq!(usage.tokens_committed, 1_000);
        assert_eq!(usage.requests_committed, 10);
    }

    #[test]
    fn consumed_rows_count_actual_usage_and_released_rows_count_nothing() {
        let mut consumed = reservation(QuotaReservationState::Reserved);
        consumed.settle(400, 3, consumed.updated_at).unwrap();
        let refunded = reservation(QuotaReservationState::Refunded);
        let mut usage = QuotaPoolUsage::default();
        usage
            .include(&consumed, &consumed.pool, consumed.window_started_at)
            .unwrap();
        usage
            .include(&refunded, &refunded.pool, refunded.window_started_at)
            .unwrap();
        assert_eq!(usage.in_flight_reservations, 0);
        assert_eq!(usage.tokens_committed, 400);
        assert_eq!(usage.requests_committed, 3);
    }

    #[test]
    fn settlement_is_bounded_and_released_quota_cannot_be_consumed() {
        let mut consumed = reservation(QuotaReservationState::Reserved);
        assert!(consumed.settle(u64::MAX, 11, consumed.updated_at).is_err());
        assert_eq!(consumed.state, QuotaReservationState::Reserved);
        consumed.settle(u64::MAX, 9, consumed.updated_at).unwrap();
        assert_eq!(consumed.tokens_consumed, consumed.tokens_reserved);
        consumed.settle(900, 9, consumed.updated_at).unwrap();
        consumed.settle(800, 8, consumed.updated_at).unwrap();
        assert_eq!(consumed.tokens_consumed, consumed.tokens_reserved);
        assert_eq!(consumed.requests_consumed, 9);

        let mut refunded = reservation(QuotaReservationState::Refunded);
        assert!(refunded.settle(1, 1, refunded.updated_at).is_err());
    }

    #[test]
    fn host_wide_capacity_aggregates_across_owners() {
        let desktop = reservation(QuotaReservationState::Reserved);
        let mut native = reservation(QuotaReservationState::Reserved);
        native.reservation_id = "quota-run-native".into();
        native.owner_id = "native-owner".into();
        native.run_id = "run-native".into();
        native.pool.owner_id = "legacy-native-owner".into();
        native.migrate_host_wide_pool();
        native.validate().unwrap();
        assert!(native.pool.owner_id.is_empty());
        assert_eq!(native.owner_id, "native-owner");
        assert_eq!(desktop.pool_id, native.pool_id);
        assert!(desktop.pool.same_capacity_pool(&native.pool));

        let mut usage = QuotaPoolUsage::default();
        usage
            .include(&desktop, &desktop.pool, desktop.window_started_at)
            .unwrap();
        usage
            .include(&native, &desktop.pool, desktop.window_started_at)
            .unwrap();
        assert_eq!(usage.in_flight_reservations, 2);

        let mut third = native.clone();
        third.reservation_id = "quota-run-3".into();
        third.run_id = "run-3".into();
        third.limits.max_in_flight_reservations = 2;
        assert!(usage.ensure_can_reserve(&third).is_err());
    }

    #[test]
    fn legacy_pool_owner_migrates_onto_reservation() {
        let json = serde_json::json!({
            "schemaVersion": QUOTA_LEDGER_SCHEMA_VERSION,
            "reservationId": "quota-legacy",
            "pool": {
                "ownerId": "primary",
                "workspace": "/workspace",
                "providerId": "xai",
                "credentialFingerprint": "cred-fingerprint",
                "class": "coding_execution"
            },
            "poolId": "stale-owner-scoped-hash",
            "runId": "run-legacy",
            "routeSnapshotHash": "route-hash",
            "limits": QuotaLimits::default(),
            "windowStartedAt": "2026-08-22T12:00:00Z",
            "tokensReserved": 1_000,
            "requestsReserved": 10,
            "tokensConsumed": 0,
            "requestsConsumed": 0,
            "state": "reserved",
            "createdAt": "2026-08-22T12:34:56Z",
            "updatedAt": "2026-08-22T12:34:56Z"
        });
        let mut loaded: QuotaReservation = serde_json::from_value(json).unwrap();
        loaded.migrate_host_wide_pool();
        loaded.validate().unwrap();
        assert_eq!(loaded.owner_id, "primary");
        assert!(loaded.pool.owner_id.is_empty());
        assert_eq!(loaded.pool_id, loaded.pool.pool_id());
    }
}
