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
//! - [`effects`] — a turn's in-flight effects, registered before they start.
//! - [`cancel`] — cancellation that reports a turn stopped only once those
//!   effects are proven idle.
//! - [`delivery`] — what a transport failure proves about delivery, so an
//!   automatic retry stands down unless non-delivery is proven. A rule, not a
//!   ledger: the durable attempt lattice is #497's G3.
//!
//! The last two are crate-internal bookkeeping over the host's own work. They
//! grant nothing and are deliberately unreachable from outside this crate, so
//! they cannot be mistaken for, or presented as, authority.
//!
//! Both are synchronous, allocation-bounded and free of I/O, so they are
//! exhaustively testable offline. Neither contacts a provider, reads a
//! credential, or opens a socket.
//!
//! Send authority, capability and operator identity, durable claims, effect
//! supervision and the audit ledger are **not** here: they belong to the
//! canonical G1-G4 host authority spine (#497), and a second public copy of any
//! of them is precisely what #478 and #492 exist to prevent.

pub(crate) mod attempt;
pub(crate) mod cancel;
pub mod delivery;
pub(crate) mod effects;
pub mod observation;
pub mod progress;

pub use delivery::{classify_transport_failure, DeliveryKnowledge};
pub use observation::{BoundedProjection, RawObservation, RawObservationDigest};
pub use progress::{
    is_wait_shaped_tool, round_is_witnessed_wait, ActiveTaskWaitWitness, ActiveWaitState,
    ProgressLedger, RepeatClass, StopDecision, StopDetail,
};
