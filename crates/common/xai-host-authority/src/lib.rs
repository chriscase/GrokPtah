//! One canonical host-issued authority spine.
//!
//! Four gates over a single durable root and a single receipt family:
//!
//! 1. **Principal root** — the host issues principal identity, credential
//!    incarnations, authentication generations, sessions, workspaces, and
//!    resource incarnations. Callers present bearers; they never mint identity
//!    and never bring a resource into existence by naming it.
//! 2. **Sealed capabilities** — a capability's scope is fixed at issue time,
//!    and a one-use [`EffectLease`] binds exactly one action digest to the
//!    observation revision it was planned against.
//! 3. **Physical-send attempt lattice** — the only way to send is to hold a
//!    [`PhysicalSendPermit`], which is bound to the full request identity
//!    (URL, method, dialect, credential, model, body) and is consumed by value.
//! 4. **Typed audit** — an append-only, hash-chained log whose ordering rules
//!    make a pre-effect write failure prevent dispatch, and make a
//!    post-dispatch write failure ambiguous rather than an ordinary failure.
//!
//! # Invariants this crate holds structurally
//!
//! * **No caller-forgeable approvals.** Every receipt has private fields and
//!   `pub(crate)` construction. `tests/ui` pins this with compile-fail cases.
//! * **No ordinary send bypass.** A physical send requires a permit, and
//!   [`HostAuthority::begin_send`] is its only producer.
//! * **Pre-effect persistence failure prevents dispatch.** The permit is
//!   constructed only after the attempt record and the intent audit record are
//!   both durable.
//! * **Post-dispatch ambiguity settles `Uncertain` and never auto-retries.**
//!   There is no retry API; the only exit is
//!   [`HostAuthority::reconcile_attempt`] with established provider truth.
//! * **Public projections are secret-, content-, and path-free.** Identifiers
//!   render as truncated, domain-separated digests; bodies, URLs, credentials
//!   and filesystem paths are digested on the way in and never stored.

//! # No caller-forgeable approvals
//!
//! Every receipt has private fields and `pub(crate)` construction, so
//! downstream code cannot build one. These cases must never start compiling.
//!
//! A receipt cannot be built with a struct literal:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = AuthContext {
//!     principal: todo!(),
//!     incarnation: todo!(),
//!     auth_generation: todo!(),
//!     capability_generation: todo!(),
//!     control_epoch: todo!(),
//!     credential_id: String::new(),
//!     owner_id: String::new(),
//! };
//! ```
//!
//! Neither can a send permit — the one thing that authorises a physical send:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = PhysicalSendPermit {
//!     attempt: todo!(),
//!     lease: todo!(),
//!     binding: todo!(),
//!     request_digest: todo!(),
//!     body_digest: todo!(),
//!     idempotency_key: String::new(),
//! };
//! ```
//!
//! Nor a sealed capability, nor a one-use lease:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = SealedCapability {
//!     id: todo!(),
//!     binding: todo!(),
//!     effect: EffectClass::ProviderSend,
//!     expires_at_ms: 0,
//! };
//! ```
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = EffectLease {
//!     id: todo!(),
//!     capability: todo!(),
//!     binding: todo!(),
//!     observation_revision: todo!(),
//!     observation_digest: todo!(),
//!     action_digest: todo!(),
//!     effect: EffectClass::ProviderSend,
//!     expires_at_ms: 0,
//! };
//! ```
//!
//! Host-issued identity cannot be minted by a caller:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = PrincipalId([0u8; 16]);
//! ```
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = ResourceIncarnation([0u8; 16]);
//! ```
//!
//! And a caller cannot claim a generation it was not issued:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = AuthGeneration(9_999);
//! ```
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = CapabilityGeneration(9_999);
//! ```
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = ControlEpoch(9_999);
//! ```
//!
//! Resolving an ambiguous effect is an operator assertion about the outside
//! world, so it needs the admin authority too - a component that merely serves
//! requests cannot declare that an uncertain send did or did not happen:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn decide(authority: &HostAuthority, attempt: AttemptId) {
//!     let _ = authority.reconcile_attempt(attempt, true);
//! }
//! ```
//!
//! Authority identity is not deserializable. A derived `Deserialize` would be
//! a public constructor in disguise, letting downstream code mint an identity
//! or claim a generation straight from JSON:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn mintable<'de, T: serde::Deserialize<'de>>() {}
//! mintable::<PrincipalId>();
//! ```
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn mintable<'de, T: serde::Deserialize<'de>>() {}
//! mintable::<AuthGeneration>();
//! ```
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn mintable<'de, T: serde::Deserialize<'de>>() {}
//! mintable::<ResourceIncarnation>();
//! ```
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn mintable<'de, T: serde::Deserialize<'de>>() {}
//! mintable::<ControlEpoch>();
//! ```
//!
//! Administering the root requires a [`HostAdminAuthority`], which only
//! [`HostAuthority::open`] produces. It cannot be forged:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let _ = HostAdminAuthority { _seal: () };
//! ```
//!
//! It is not `Clone`, so it cannot be duplicated into a component that was
//! only meant to serve requests:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn duplicate(admin: &HostAdminAuthority) -> HostAdminAuthority {
//!     admin.clone()
//! }
//! ```
//!
//! And a component holding only `&HostAuthority` cannot replace the
//! credential set:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn replace(authority: &HostAuthority, credentials: &[HostCredential]) {
//!     let _ = authority.set_credentials(credentials, "account-1");
//! }
//! ```
//!
//! An ambiguous outcome cannot be downgraded to an ordinary failure: there is
//! no conversion from [`UncertainReason`] to [`FailedReason`].
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! let uncertain = UncertainReason::AuditNotDurableAfterDispatch;
//! let _: FailedReason = uncertain.into();
//! ```
//!
//! A permit is consumed by value at settlement, so it cannot be spent twice:
//!
//! ```compile_fail
//! # use xai_host_authority::*;
//! fn spend_twice(authority: &HostAuthority, permit: PhysicalSendPermit) {
//!     let _ = authority.settle_settled(permit);
//!     let _ = authority.settle_settled(permit);
//! }
//! ```

mod audit;
mod digest;
mod error;
mod gates;
mod ids;
mod receipt;
mod state;
mod store;

pub use audit::{AuditEvent, AuditRecord};
pub use digest::{ContentDigest, RequestIdentity};
pub use error::AuthorityError;
pub use gates::AttemptProjection;
pub use ids::{
    AttemptId, AuthGeneration, CapabilityGeneration, CapabilityId, ControlEpoch,
    CredentialIncarnation, EffectLeaseId, ObservationRevision, PrincipalId, ResourceIncarnation,
    SessionId, WorkspaceId,
};
pub use receipt::{
    ActorClass, AuthContext, AuthorityBinding, EffectClass, EffectLease, FailedReason,
    PhysicalSendPermit, SealedCapability, SendOutcome, UncertainReason,
};
pub use store::{HostAdminAuthority, HostAuthority, HostCredential};

pub(crate) use store::unix_time_millis;
