//! Request digest parity and closed-contract enforcement.

use crate::dto::*;

fn chunk(chunk_id: &str, sources: &[&str]) -> AnswerContextChunk {
    AnswerContextChunk {
        chunk_id: chunk_id.into(),
        article_id: "operations.durable-recovery".into(),
        chunk_digest: "sha256:chunk".into(),
        text: "Recover a durable run safely".into(),
        source_ids: sources.iter().map(|id| (*id).to_string()).collect(),
    }
}

fn core() -> AnswerRequestCore {
    AnswerRequestCore {
        schema: HELP_ANSWER_REQUEST_SCHEMA.into(),
        query: "durable run recovery".into(),
        corpus_digest: "sha256:corpus".into(),
        index_digest: "sha256:index".into(),
        context: vec![chunk("c1", &["durable.lifecycle"])],
        instruction: "Answer only from the supplied Help context.".into(),
        tools_disabled: true,
        conversation_disabled: true,
        max_answer_chars: 4_000,
    }
}

#[test]
fn the_request_digest_follows_every_field() {
    let base = request_digest(&core());
    for mutate in [
        (|c: &mut AnswerRequestCore| c.query = "something else".into())
            as fn(&mut AnswerRequestCore),
        |c: &mut AnswerRequestCore| c.corpus_digest = "sha256:other".into(),
        |c: &mut AnswerRequestCore| c.index_digest = "sha256:other".into(),
        |c: &mut AnswerRequestCore| c.instruction = "Do whatever you like.".into(),
        |c: &mut AnswerRequestCore| c.max_answer_chars = 4_001,
        |c: &mut AnswerRequestCore| c.context.clear(),
        |c: &mut AnswerRequestCore| c.context[0].text = "Recover a durable run freely".into(),
        |c: &mut AnswerRequestCore| c.context[0].chunk_digest = "sha256:substituted".into(),
        |c: &mut AnswerRequestCore| c.context.push(chunk("c2", &["durable.lifecycle"])),
    ] {
        let mut edited = core();
        mutate(&mut edited);
        assert_ne!(base, request_digest(&edited));
    }
}

#[test]
fn source_id_order_does_not_change_the_digest_but_membership_does() {
    let mut forward = core();
    forward.context[0].source_ids = vec!["a".into(), "b".into()];
    let mut reversed = core();
    reversed.context[0].source_ids = vec!["b".into(), "a".into()];
    assert_eq!(request_digest(&forward), request_digest(&reversed));

    let mut extra = core();
    extra.context[0].source_ids = vec!["a".into(), "b".into(), "c".into()];
    assert_ne!(request_digest(&forward), request_digest(&extra));
}

#[test]
fn moving_text_between_adjacent_fields_changes_the_digest() {
    // The length-prefixed encoding is what makes this true: a separator-joined
    // encoding would let a boundary move without the bytes changing.
    let mut left = core();
    left.context[0].chunk_id = "ab".into();
    left.context[0].article_id = "c".into();
    let mut right = core();
    right.context[0].chunk_id = "a".into();
    right.context[0].article_id = "bc".into();
    assert_ne!(request_digest(&left), request_digest(&right));
}

#[test]
fn a_request_that_does_not_disable_tools_is_not_this_contract() {
    let mut with_tools = core();
    with_tools.tools_disabled = false;
    assert_eq!(
        with_tools.enforce(),
        Err(ContractError::NotBounded("toolsDisabled"))
    );

    let mut with_history = core();
    with_history.conversation_disabled = false;
    assert_eq!(
        with_history.enforce(),
        Err(ContractError::NotBounded("conversationDisabled"))
    );
}

#[test]
fn an_unknown_field_is_refused_rather_than_dropped() {
    // A dropped `claimIndex` would silently turn claim-bound coverage back
    // into the aggregate ratio it replaced.
    let raw =
        r#"{"claimIndex":0,"chunkId":"c1","articleId":"a","sourceId":"s","quote":"q","extra":1}"#;
    assert!(serde_json::from_str::<AnswerCitationInput>(raw).is_err());
}

#[test]
fn a_reply_naming_another_admission_or_corpus_is_refused() {
    let core = core();
    let good = AnswerReply {
        schema: HELP_ANSWER_RESPONSE_SCHEMA.into(),
        answer: "Recover a durable run safely.".into(),
        citations: vec![AnswerCitationInput {
            claim_index: 0,
            chunk_id: "c1".into(),
            article_id: "operations.durable-recovery".into(),
            source_id: "durable.lifecycle".into(),
            quote: "Recover a durable run safely".into(),
        }],
        uncertainty: "Live state must be re-checked.".into(),
        corpus_digest: core.corpus_digest.clone(),
        admission_id: "sha256:admission".into(),
    };
    assert_eq!(good.enforce(&core, "sha256:admission"), Ok(()));

    let mut wrong_admission = good.clone();
    wrong_admission.admission_id = "sha256:other".into();
    assert!(wrong_admission.enforce(&core, "sha256:admission").is_err());

    let mut wrong_corpus = good.clone();
    wrong_corpus.corpus_digest = "sha256:other".into();
    assert!(wrong_corpus.enforce(&core, "sha256:admission").is_err());

    let mut outside = good.clone();
    outside.citations[0].chunk_id = "never-retrieved".into();
    assert!(outside.enforce(&core, "sha256:admission").is_err());

    let mut uncited = good.clone();
    uncited.citations.clear();
    assert!(uncited.enforce(&core, "sha256:admission").is_err());

    let mut silent = good;
    silent.uncertainty = "   ".into();
    assert!(silent.enforce(&core, "sha256:admission").is_err());
}
