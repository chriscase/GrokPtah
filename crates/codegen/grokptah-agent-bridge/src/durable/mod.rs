//! Durable agent core: the semantics a long-running agent needs to be safe.
//!
//! Part of the #492 consolidation plan's durable-agent train, scoped to the one
//! thing that train can own without an authority root: deciding, honestly,
//! whether a long-running turn is making progress.
//!
//! It holds **no authority of its own**. Host lifecycle, principal identity,
//! capability generations, the physical-send lattice and the audit chain all
//! belong to the canonical G1-G4 spine (#497), and nothing here duplicates,
//! approximates or stands in for any of them.
//!
//! # What this module holds
//!
//! - [`observation`] — raw digests taken *before* any bounded projection of the
//!   output, with the digest kept opaque so it can never reach a durable record.
//! - [`progress`] — stationarity that separates a stuck turn from a productive
//!   wait, using the raw observation and a host-issued wait witness.
//!
//! Both are synchronous, allocation-bounded and free of I/O, so they are
//! exhaustively testable offline. Neither contacts a provider, reads a
//! credential, or opens a socket.
//!
//! Send authority, capability and operator identity, durable claims, effect
//! supervision and the audit ledger are **not** here: they belong to the
//! canonical G1-G4 host authority spine (#497), and a second public copy of any
//! of them is precisely what #478 and #492 exist to prevent.

pub mod observation;
pub mod progress;

pub use observation::{BoundedProjection, RawObservation, RawObservationDigest};
pub use progress::{
    is_wait_shaped_tool, round_is_witnessed_wait, ActiveTaskWaitWitness, ActiveWaitState,
    ProgressLedger, RepeatClass, StopDecision, StopDetail,
};
