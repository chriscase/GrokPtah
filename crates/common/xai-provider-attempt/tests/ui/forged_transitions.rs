fn main() {
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
