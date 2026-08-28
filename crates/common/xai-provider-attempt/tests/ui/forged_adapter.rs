fn main() {
    let _ = xai_provider_attempt::CanonicalHostAuthority::from_trusted_host_adapter(
        "forged",
        1,
        1,
        "forged-lease",
        "forged-scope",
    );
    let _ = xai_provider_attempt::AttemptContext::from_host_authority(
        todo!(),
        "forged-operation",
        todo!(),
        todo!(),
    );
}
