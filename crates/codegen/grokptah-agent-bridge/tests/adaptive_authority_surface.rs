//! Out-of-crate negative surface checks.
//!
//! The authority adapter is intentionally crate-private until the canonical
//! #477/#458/#478 assembly lands. This test is compiled as an external
//! integration crate, so accidentally exporting the seam or an install-anything
//! bypass fails the relevant gate.

#[test]
fn authority_seam_and_install_hook_are_not_public_api() {
    let seam = include_str!("../src/computer_profile/authority_seam.rs");
    let module = include_str!("../src/computer_profile/mod.rs");
    let host = include_str!("../src/host.rs");
    let root = include_str!("../src/lib.rs");

    assert!(seam.contains("pub(crate) trait AdaptiveAuthorityAdapter"));
    assert!(!module.contains("pub mod authority"));
    assert!(!module.contains("pub use authority"));
    assert!(!root.contains("AdaptiveAuthorityAdapter"));
    assert!(host.contains("pub(crate) fn install_adaptive_authority_adapter"));
    assert!(!host.contains("pub fn install_canonical_adaptive_authority"));
    for removed in [
        "CanonicalAuthority",
        "PrincipalGenerationRef",
        "CapabilityGenerationRef",
        "AdaptiveAuthoritySnapshot",
        "ProviderAttemptReceipt",
    ] {
        assert!(
            !seam.contains(&format!("pub struct {removed}")),
            "provisional authority type {removed} was reintroduced"
        );
    }
}

#[test]
fn volatile_session_measurement_cannot_be_action_authority() {
    use grokptah_agent_bridge::{
        CapabilityAttribution, CapabilityEvidence, ComputerUseTier, HostCapabilityEvidence,
        ModelCapabilityEvidence,
    };

    let evidence = CapabilityEvidence::synthetic(
        ModelCapabilityEvidence {
            tools: true,
            image_input: false,
            max_image_bytes: None,
            tier: ComputerUseTier::SemanticAct,
            attribution: CapabilityAttribution::Measured,
            durable_authority: false,
            session_measured: true,
            synthetic_only: true,
        },
        HostCapabilityEvidence::SEMANTIC_ONLY,
    );
    assert!(!evidence.may_propose());
    assert_eq!(
        evidence.ceiling(),
        grokptah_agent_bridge::AdaptiveProfile::Economy
    );
}
