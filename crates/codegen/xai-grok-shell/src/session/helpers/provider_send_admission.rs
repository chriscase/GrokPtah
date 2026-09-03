//! Structural gate: production model-generation helpers/callers go through
//! the admitted sampler client. No second transport and no raw `execute`/`send`.
//!
//! Inventory is the #478 caller-audit model-generation set only. Metadata,
//! feedback, remote, tool, storage, and test-only HTTP are out of scope.

fn assert_no_raw_provider_send(name: &str, source: &str) {
    assert!(
        !source.contains("client.execute("),
        "{name} must not call client.execute; model sends go through SamplingClient"
    );
    assert!(
        !source.contains("self.http.execute"),
        "{name} must not execute HTTP outside the admitted sampler client"
    );
    assert!(
        !source.contains(".send().await"),
        "{name} must not raw-send; model sends go through SamplingClient"
    );
}

#[test]
fn helper_model_callers_delegate_to_admitted_sampler_client() {
    let compact = include_str!("session_compact.rs");
    let summary = include_str!("session_summary.rs");
    let chat = include_str!("chat.rs");
    let flush = include_str!("memory_flush.rs");
    let suggest = include_str!("prompt_suggest.rs");
    let recap = include_str!("session_recap.rs");
    let full_replace = include_str!("full_replace_compaction.rs");
    let image_describe = include_str!("../image_describe.rs");
    let laziness = include_str!("../acp_session_impl/laziness.rs");
    let sampler_turn = include_str!("../acp_session_impl/sampler_turn.rs");
    let trace_classifier = include_str!("../../trace_classifier/mod.rs");
    let prompt_build = include_str!("../acp_session_impl/prompt_build.rs");
    let compaction = include_str!("../compaction.rs");
    let recap_caller = include_str!("../acp_session_impl/recap.rs");
    let memory_dream = include_str!("../acp_session_impl/memory_dream.rs");

    for (name, source) in [
        ("session_compact.rs", compact),
        ("session_summary.rs", summary),
        ("chat.rs", chat),
        ("memory_flush.rs", flush),
        ("prompt_suggest.rs", suggest),
        ("session_recap.rs", recap),
        ("full_replace_compaction.rs", full_replace),
        ("image_describe.rs", image_describe),
        ("acp_session_impl/laziness.rs", laziness),
        ("acp_session_impl/sampler_turn.rs", sampler_turn),
        ("trace_classifier/mod.rs", trace_classifier),
        ("acp_session_impl/prompt_build.rs", prompt_build),
        ("compaction.rs", compaction),
        ("acp_session_impl/recap.rs", recap_caller),
        ("acp_session_impl/memory_dream.rs", memory_dream),
    ] {
        assert_no_raw_provider_send(name, source);
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
    assert!(
        full_replace.contains("generate_session_compact("),
        "full-replace compaction must sample through generate_session_compact"
    );
    assert!(
        image_describe.contains("client.conversation_collect("),
        "image describe must collect through SamplingClient::conversation_collect"
    );
    assert!(
        laziness.contains("prepare_chat_completion") && laziness.contains("conversation_collect("),
        "laziness classifier must collect through the admitted sampler client"
    );
    assert!(
        sampler_turn.contains("xai_grok_sampler::SamplingClient")
            && sampler_turn.contains("prepare_chat_completion")
            && sampler_turn.contains("conversation_collect(")
            && sampler_turn.contains("sampler_handle")
            && sampler_turn.contains("submit_and_collect("),
        "sampler turn must use SamplingClient / admitted sampler-handle methods"
    );
    assert!(
        trace_classifier.contains("xai_grok_sampler::SamplingClient")
            && trace_classifier.contains("conversation_collect("),
        "trace classifier must collect through SamplingClient::conversation_collect"
    );
    assert!(
        prompt_build.contains("xai_grok_sampler::SamplingClient::new")
            && prompt_build.contains("get_or_describe"),
        "prompt_build must construct SamplingClient and describe through the admitted helper"
    );
    assert!(
        compaction.contains("prepare_chat_completion")
            && compaction.contains("generate_session_compact")
            && compaction.contains("ShellCompactionSampler"),
        "compaction must prepare a SamplingClient and sample through generate_session_compact"
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
        assert_no_raw_provider_send(name, source);
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
