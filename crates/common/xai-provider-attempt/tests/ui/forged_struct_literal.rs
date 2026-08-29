fn main() {
    let _ = xai_provider_attempt::CanonicalHostAuthority {
        principal_incarnation: "forged".into(),
        auth_generation: 1,
        capability_generation: 1,
        effect_lease_id: "forged-lease".into(),
        effect_scope: "forged-scope".into(),
    };
}
