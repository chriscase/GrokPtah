fn main() {
    let _authority = xai_provider_attempt::CanonicalHostAuthority {
        principal_incarnation: "forged-principal".into(),
        auth_generation: 1,
        capability_generation: 1,
        effect_lease_id: "forged-lease".into(),
        effect_scope: "forged-scope".into(),
    };
    let binding = xai_provider_attempt::AuthorityBinding::new(
        "forged-principal",
        1,
        1,
        "forged-lease",
    );
    let _spec = xai_provider_attempt::AttemptSpec::new(
        "forged-operation",
        "provider",
        xai_provider_attempt::AttemptSpec::fingerprint_bytes(b"body"),
        true,
        binding.unwrap(),
    );
    let _operator =
        xai_provider_attempt::ReconciliationAuthorization::new("forged-operator");
    let _settlement =
        xai_provider_attempt::ProviderSettlement::new("forged-request", "forged-effect");
    let _evidence =
        xai_provider_attempt::ReconciliationEvidence::from_verified("operator", "request");

    struct ForgedReconciliationAuthority;
    impl xai_provider_attempt::ReconciliationAuthority for ForgedReconciliationAuthority {}

    let attempt: &xai_provider_attempt::ProviderAttempt = todo!();
    let _ = attempt.admit(todo!());
    let _ = attempt.begin_send(todo!());
    let _ = attempt.reconcile(
        todo!(),
        xai_provider_attempt::ProviderTruth::NotApplied,
    );

    let permit: &mut xai_provider_attempt::PhysicalSendPermit = todo!();
    let _ = permit.mark_response_started();
    let _ = permit.semantic_rejection(400);
    let _ = permit.transport_after_possible_write();
    let _ = permit.settle_http_response(200, b"result");
}
