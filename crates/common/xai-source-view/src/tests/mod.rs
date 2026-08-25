//! Test suite for read-only source inspection.
//!
//! Every fixture is synthetic and built inside a fresh temporary directory; no
//! test reads repository content, and no test depends on wall-clock time.

mod authority;
mod containment;
mod contract;
mod races;
mod reads;
mod support;
