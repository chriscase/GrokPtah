//! Structural gate: session helper model calls go through the admitted
//! sampler client. No second transport and no raw `execute`/`send`.

#[test]
fn helper_model_callers_delegate_to_admitted_sampler_client() {
    let compact = include_str!("session_compact.rs");
    let summary = include_str!("session_summary.rs");
    let chat = include_str!("chat.rs");
    let flush = include_str!("memory_flush.rs");
    let suggest = include_str!("prompt_suggest.rs");
    let recap = include_str!("session_recap.rs");

    for (name, source) in [
        ("session_compact.rs", compact),
        ("session_summary.rs", summary),
        ("chat.rs", chat),
        ("memory_flush.rs", flush),
        ("prompt_suggest.rs", suggest),
        ("session_recap.rs", recap),
        (
            "full_replace_compaction.rs",
            include_str!("full_replace_compaction.rs"),
        ),
    ] {
        assert!(
            !source.contains("client.execute("),
            "{name} must not call client.execute; helper sends go through SamplingClient"
        );
        assert!(
            !source.contains("self.http.execute"),
            "{name} must not execute HTTP outside the admitted sampler client"
        );
        assert!(
            !source.contains(".send().await"),
            "{name} must not raw-send; helper sends go through SamplingClient"
        );
    }

    let compact_prod = compact
        .split("#[cfg(test)]")
        .next()
        .expect("compact production source");
    assert!(
        compact_prod.contains("client.chat_completion_stream(")
            && compact_prod.contains("client.conversation_stream_responses(")
            && compact_prod.contains("client.conversation_stream_messages("),
        "compaction must dispatch through SamplingClient stream methods"
    );
    assert!(
        summary.contains("client.conversation_collect("),
        "session summary must dispatch through SamplingClient::conversation_collect"
    );
    assert!(
        chat.contains("sampling_client.conversation_collect("),
        "text_completion must dispatch through SamplingClient::conversation_collect"
    );
    assert!(
        !flush.contains("conversation_collect(") && !flush.contains("chat_completion"),
        "memory_flush helper is prompt-only; the SessionActor issues the admitted send"
    );
    assert!(
        !suggest.contains("conversation_collect(") && !suggest.contains("chat_completion"),
        "prompt_suggest helper is prompt-only; the SessionActor issues the admitted send"
    );
    assert!(
        !recap.contains("conversation_collect(") && !recap.contains("chat_completion"),
        "session_recap helper is prompt-only; the SessionActor issues the admitted send"
    );
}

#[test]
fn flush_and_suggest_session_callers_use_admitted_sampler_client() {
    let suggest_caller = include_str!("../acp_session_impl/recap.rs");
    let flush_caller = include_str!("../acp_session_impl/memory_dream.rs");
    for (name, source) in [
        ("acp_session_impl/recap.rs", suggest_caller),
        ("acp_session_impl/memory_dream.rs", flush_caller),
    ] {
        assert!(
            !source.contains("client.execute(")
                && !source.contains("self.http.execute")
                && !source.contains(".send().await"),
            "{name} must not raw-send model requests"
        );
        assert!(
            source.contains("conversation_collect("),
            "{name} must send through SamplingClient::conversation_collect"
        );
    }
    assert!(
        suggest_caller.contains("handle_suggest_prompt")
            && suggest_caller.contains("sampling_client.conversation_collect("),
        "prompt suggestion must collect through the admitted sampler client"
    );
    assert!(
        flush_caller.contains("run_memory_flush")
            && flush_caller.contains("sampling_client")
            && flush_caller.contains("conversation_collect("),
        "memory flush must collect through the admitted sampler client"
    );
}
