fn main() {
    let _ = xai_provider_attempt::AuthorityBinding::new(
        "forged",
        1,
        1,
        "forged-lease",
        "forged-scope",
    );
    let _ = xai_provider_attempt::AttemptSpec::new(
        "forged-operation",
        "provider",
        xai_provider_attempt::AttemptSpec::fingerprint_bytes(b"body"),
        true,
        todo!(),
    );
    let _ = xai_provider_attempt::ReconciliationAuthorization::new("forged-operator");
    let _ = xai_provider_attempt::ProviderSettlement::new("request", "effect");
    let _ = xai_provider_attempt::ReconciliationEvidence::from_verified("operator", "request");
}
