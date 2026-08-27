//! The synthetic efficiency benchmark.
//!
//! Everything in this module is deterministic and in-process. There is no
//! hardware, no virtual machine, no provider, no image model, and no operator;
//! the numbers a run produces are synthetic accounting units, not
//! measurements. That constraint is what makes the benchmark useful for the
//! thing it is actually for -- comparing *contracts* across profiles, model
//! tiers, and horizons -- and it is stated on every receipt through
//! [`crate::vocabulary::NotClaimed`].
//!
//! The pieces:
//!
//! * [`rng`] -- reproducible variation, so a trace is evidence rather than an
//!   anecdote.
//! * [`world`] -- the synthetic application and its scripted perturbations.
//! * [`scenario`] -- the hazard families, including two negative controls for
//!   runs that are too timid rather than too reckless.
//! * [`agent`] -- a reference planner, deliberately naive in one respect so
//!   the executor's guards are what is under test.
//! * [`runner`] -- the planner/executor loop with budget, retry, escalation,
//!   approval, and cancellation handling.
//! * [`suite`] -- the full profile x tier x horizon matrix and its gates.

pub mod agent;
pub mod rng;
pub mod runner;
pub mod scenario;
pub mod suite;
pub mod world;
