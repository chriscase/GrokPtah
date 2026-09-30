//! Narrow Responses protocol for the host-sealed managed Grok Build profile.
//! xAI documents max_output_tokens as inclusive of reasoning. No Chat
//! Completions receipt or client projection establishes that bound.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResponsesUsageSnapshot {
    pub receipt_state: ManagedObservedContainer,
    pub input_details_state: ManagedObservedContainer,
    pub output_details_state: ManagedObservedContainer,
    pub fields: [ManagedObservedUsageField; 8],
    pub unknown_top_level_keys: Option<ManagedUnknownUsageKeys>,
    pub unknown_input_details_keys: Option<ManagedUnknownUsageKeys>,
    pub unknown_output_details_keys: Option<ManagedUnknownUsageKeys>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResponsesUsageObservation {
    pub snapshots: Vec<ResponsesUsageSnapshot>,
    pub observations_omitted: bool,
    pub credential_suppressed: bool,
}

const USAGE_KEYS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "total_tokens",
    "input_tokens_details",
    "output_tokens_details",
    "cost_in_usd_ticks",
    "num_sources_used",
    "num_server_side_tools_used",
];

pub(super) fn observe(value: &Value) -> ResponsesUsageSnapshot {
    use ManagedObservedUsagePath as Path;
    let top = value.as_object();
    let input_parent = value.get("input_tokens_details");
    let output_parent = value.get("output_tokens_details");
    let input = input_parent.and_then(Value::as_object);
    let output = output_parent.and_then(Value::as_object);
    let fields = [
        (Path::InputTokens, top, "input_tokens"),
        (Path::OutputTokens, top, "output_tokens"),
        (Path::TotalTokens, top, "total_tokens"),
        (Path::InputCachedTokens, input, "cached_tokens"),
        (Path::OutputReasoningTokens, output, "reasoning_tokens"),
        (Path::NumSourcesUsed, top, "num_sources_used"),
        (Path::ServerSideToolsUsed, top, "num_server_side_tools_used"),
        (Path::CostInUsdTicks, top, "cost_in_usd_ticks"),
    ]
    .map(|(path, object, key)| {
        observed_usage_field(path, object.and_then(|o| o.get(key)), object.is_some())
    });
    ResponsesUsageSnapshot {
        receipt_state: observed_container(Some(value)),
        input_details_state: observed_container(input_parent),
        output_details_state: observed_container(output_parent),
        fields,
        unknown_top_level_keys: top.map(|o| observed_unknown_keys(o, USAGE_KEYS)),
        unknown_input_details_keys: input.map(|o| observed_unknown_keys(o, &["cached_tokens"])),
        unknown_output_details_keys: output
            .map(|o| observed_unknown_keys(o, &["reasoning_tokens"])),
    }
}

impl ManagedProviderEvidence {
    pub(super) fn retain_responses_observation(&mut self, snapshot: ResponsesUsageSnapshot) {
        let observation = self
            .responses_usage_observation
            .get_or_insert_with(Default::default);
        if observation.snapshots.len() >= MAX_USAGE_OBSERVATIONS
            || !serde_json::to_vec(&snapshot).is_ok_and(|v| v.len() <= MAX_USAGE_SNAPSHOT_BYTES)
        {
            observation.observations_omitted = true;
        } else {
            observation.snapshots.push(snapshot);
        }
    }
}

pub(super) fn validate_request(body: &Value, model: &str, cap: u32) -> Result<(), &'static str> {
    const KEYS: &[&str] = &[
        "model",
        "input",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "stream",
        "max_output_tokens",
        "reasoning",
        "include",
        "store",
        "prompt_cache_key",
    ];
    let object = body
        .as_object()
        .ok_or("managed Responses request is not an object")?;
    if object.keys().any(|k| !KEYS.contains(&k.as_str()))
        || body["model"].as_str() != Some(model)
        || body["stream"] != true
        || !body["input"]
            .as_array()
            .is_some_and(|items| items.iter().all(safe_input))
        || cap == 0
        || cap > MAX_OUTPUT_TOKENS
        || body["max_output_tokens"].as_u64() != Some(u64::from(cap))
        || body.get("reasoning") != Some(&json!({"summary":"concise"}))
        || body.get("include") != Some(&json!(["reasoning.encrypted_content"]))
        || body.get("store") != Some(&json!(false))
        || body
            .get("parallel_tool_calls")
            .is_some_and(|v| v != &json!(false))
        || body.get("tool_choice").is_some_and(|v| v != &json!("auto"))
    {
        return Err("managed Responses route or sealed bound disagrees");
    }
    let tools = body["tools"]
        .as_array()
        .ok_or("managed Responses tools are missing")?;
    let mut names = BTreeSet::new();
    if tools.len() > 7
        || tools.iter().any(|t| {
            t["type"] != "function"
                || !file_tool(t["name"].as_str().unwrap_or_default())
                || !names.insert(t["name"].as_str().unwrap_or_default())
                || !t["parameters"].is_object()
        })
    {
        return Err("managed Responses tools exceed file authority");
    }
    Ok(())
}

// Only text and the pinned client-side conversation items enter this profile.
// Image/remote-content inputs would invalidate a byte-sized input reservation.
fn text_content(value: &Value) -> bool {
    value.is_string()
        || value.as_array().is_some_and(|parts| {
            parts.iter().all(|part| {
                matches!(
                    part["type"].as_str(),
                    Some("input_text" | "output_text" | "summary_text")
                ) && part["text"].is_string()
            })
        })
}
fn safe_input(item: &Value) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    let keys: &[&str] = match item["type"].as_str() {
        None | Some("message")
            if matches!(
                item["role"].as_str(),
                Some("system" | "developer" | "user" | "assistant")
            ) && text_content(&item["content"]) =>
        {
            &["type", "role", "content", "id", "status"]
        }
        Some("function_call")
            if item["call_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty() && id.len() <= 128)
                && safe_file_call(
                    item["name"].as_str().unwrap_or_default(),
                    item["arguments"].as_str().unwrap_or_default(),
                ) =>
        {
            &["type", "id", "call_id", "name", "arguments", "status"]
        }
        Some("function_call_output")
            if item["call_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty() && id.len() <= 128)
                && text_content(&item["output"]) =>
        {
            &["type", "call_id", "output"]
        }
        Some("reasoning")
            if item["summary"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .all(|p| p["type"] == "summary_text" && p["text"].is_string())
            }) && item
                .get("encrypted_content")
                .is_none_or(|v| v.is_null() || v.is_string()) =>
        {
            &["type", "id", "summary", "encrypted_content", "status"]
        }
        _ => return false,
    };
    object.keys().all(|key| keys.contains(&key.as_str()))
}

fn reasoning_key(item: &Value) -> Result<(String, String), &'static str> {
    let id = item["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or("managed Responses reasoning id unsupported")?;
    if !safe_input(item) {
        return Err("managed Responses reasoning shape unsupported");
    }
    // Plaintext summaries are already covered by serialized input bytes.
    // Seal the opaque content itself; client summary normalization grants no
    // extra hidden context.
    let content = json!({"encrypted_content":item["encrypted_content"]});
    Ok((
        id.to_owned(),
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&content)
                    .map_err(|_| "managed Responses reasoning serialization failed")?
            )
        ),
    ))
}

pub(super) fn reasoning_reuse_allowance(
    body: &Value,
    owned: &BTreeMap<String, String>,
    prior_output: u64,
) -> Result<u64, &'static str> {
    let mut seen = BTreeSet::new();
    for item in body["input"]
        .as_array()
        .ok_or("managed Responses input missing")?
    {
        if item["type"] == "reasoning" {
            let (id, digest) = reasoning_key(item)?;
            if !seen.insert(id.clone()) || owned.get(&id) != Some(&digest) {
                return Err("managed Responses foreign or repeated reasoning context");
            }
        }
    }
    // Opaque content may encode previously generated context. Its allowance
    // comes from this lease's settled output, never its ciphertext byte size.
    Ok(if seen.is_empty() { 0 } else { prior_output })
}

fn file_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "write" | "search_replace" | "list_dir" | "grep" | "search_tool" | "use_tool"
    )
}

fn safe_file_call(name: &str, arguments: &str) -> bool {
    if !file_tool(name) || arguments.len() > 16 * 1024 {
        return false;
    }
    let Ok(args) = serde_json::from_str::<Value>(arguments) else {
        return false;
    };
    if !args.is_object() {
        return false;
    }
    // The pinned CLI includes two discovery functions even with --tools.
    // Discovery cannot delegate execution to a non-file tool or itself.
    name != "use_tool"
        || args["tool_name"].as_str().is_some_and(|n| {
            matches!(
                n,
                "read_file" | "write" | "search_replace" | "list_dir" | "grep"
            )
        })
}

pub(super) fn parse_usage(
    value: &Value,
    cap: u32,
) -> Result<ManagedUsage, ManagedUsageValidationFailure> {
    use ManagedUsageField as Field;
    use ManagedUsageRejectionKind as Kind;
    let fail = |kind, field| {
        ManagedUsageValidationFailure::new(
            "managed Responses usage is unsupported",
            kind,
            Some(field),
        )
    };
    let object = value.as_object().ok_or_else(|| {
        fail(Kind::MalformedReceipt, Field::TotalTokens).with_value_state(Some(value))
    })?;
    if object.keys().any(|k| !USAGE_KEYS.contains(&k.as_str())) {
        return Err(fail(Kind::UnsupportedField, Field::TotalTokens));
    }
    let number = |v: Option<&Value>, field| -> Result<u64, ManagedUsageValidationFailure> {
        v.ok_or_else(|| fail(Kind::MissingField, field).with_value_state(None))?
            .as_u64()
            .ok_or_else(|| fail(Kind::MalformedField, field).with_value_state(v))
    };
    let input = number(object.get("input_tokens"), Field::InputTokens)?;
    let output = number(object.get("output_tokens"), Field::OutputTokens)?;
    let total = number(object.get("total_tokens"), Field::TotalTokens)?;
    if input > MAX_REQUEST_BYTES as u64 {
        return Err(fail(Kind::TokenBoundExceeded, Field::InputTokens)
            .with_numbers(input, MAX_REQUEST_BYTES as u64));
    }
    if cap == 0 || cap > MAX_OUTPUT_TOKENS || output > u64::from(cap) {
        return Err(fail(Kind::TokenBoundExceeded, Field::OutputTokens)
            .with_numbers(output, u64::from(cap)));
    }
    let expected = input
        .checked_add(output)
        .ok_or_else(|| fail(Kind::AggregationOverflow, Field::TotalTokens))?;
    if expected != total {
        return Err(fail(Kind::ConflictingTotal, Field::TotalTokens).with_numbers(total, expected));
    }
    let detail = |key: &str,
                  leaf: &str,
                  parent_field,
                  leaf_field|
     -> Result<u64, ManagedUsageValidationFailure> {
        let nested = object.get(key).and_then(Value::as_object).ok_or_else(|| {
            fail(Kind::MalformedDetails, parent_field).with_value_state(object.get(key))
        })?;
        if nested.keys().any(|k| k != leaf) {
            return Err(fail(Kind::UnsupportedDetailsField, parent_field));
        }
        number(nested.get(leaf), leaf_field)
    };
    let cached = detail(
        "input_tokens_details",
        "cached_tokens",
        Field::InputTokensDetails,
        Field::CachedTokens,
    )?;
    let reasoning = detail(
        "output_tokens_details",
        "reasoning_tokens",
        Field::OutputTokensDetails,
        Field::ReasoningTokens,
    )?;
    for (component, bound, field) in [
        (cached, input, Field::CachedTokens),
        (reasoning, output, Field::ReasoningTokens),
    ] {
        if component > bound {
            return Err(fail(Kind::ConflictingSubset, field).with_numbers(component, bound));
        }
    }
    for (key, field) in [
        ("num_sources_used", Field::NumSourcesUsed),
        ("num_server_side_tools_used", Field::ServerSideToolsUsed),
    ] {
        if let Some(v) = object.get(key) {
            if v.as_u64() != Some(0) {
                return Err(fail(Kind::UnsupportedNonzeroDetail, field).with_value_state(Some(v)));
            }
        }
    }
    let cost = match object.get("cost_in_usd_ticks") {
        None => None,
        Some(v) => Some(v.as_i64().filter(|n| *n >= 0).ok_or_else(|| {
            fail(Kind::CostTypeUnsupported, Field::CostInUsdTicks).with_value_state(Some(v))
        })?),
    };
    Ok(ManagedUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: total,
        cache_read_input_tokens: cached,
        reasoning_tokens: reasoning,
        cost_in_usd_ticks: cost,
    })
}

pub(super) type Summary = (
    u32,
    ManagedUsage,
    ResponsesUsageSnapshot,
    bool,
    BTreeMap<String, String>,
);

pub(super) fn validate_completion(
    bytes: &[u8],
    child: &str,
    upstream: &str,
    cap: u32,
    model: &str,
) -> Result<Summary, CompletionValidationFailure> {
    let mut observation = None;
    validate_completion_inner(bytes, child, upstream, cap, model, &mut observation).map_err(
        |mut failure| {
            if !failure.credential_suppressed && failure.responses_observation.is_none() {
                failure.responses_observation = observation.map(Box::new);
            }
            failure
        },
    )
}

fn validate_completion_inner(
    bytes: &[u8],
    child: &str,
    upstream: &str,
    cap: u32,
    model: &str,
    observation: &mut Option<ResponsesUsageSnapshot>,
) -> Result<Summary, CompletionValidationFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| "managed Responses stream is not UTF-8")?;
    if text.contains(child) || text.contains(upstream) {
        let mut failure: CompletionValidationFailure =
            "managed Responses stream contains credential material".into();
        failure.credential_suppressed = true;
        return Err(failure);
    }
    let mut terminal = None;
    let mut event_name = None;
    let mut sequence = None;
    let mut streamed_tools = std::collections::BTreeMap::<String, Value>::new();
    let mut arguments = std::collections::BTreeMap::<String, String>::new();
    let mut done = false;
    let mut arguments_done = BTreeSet::new();
    let mut items_done = BTreeSet::new();
    let mut whole_call_delta = BTreeSet::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("event:").map(str::trim) {
            event_name = Some(name);
            continue;
        }
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            if terminal.is_none() || done {
                return Err("managed Responses ended before receipt or repeated end".into());
            }
            done = true;
            continue;
        }
        if terminal.is_some() {
            return Err("managed Responses continued after terminal receipt".into());
        }
        let v: Value =
            serde_json::from_str(data).map_err(|_| "managed Responses stream is malformed")?;
        let kind = v["type"]
            .as_str()
            .ok_or("managed Responses event type missing")?;
        if event_name.take().is_some_and(|name| name != kind) {
            return Err("managed Responses event type disagrees".into());
        }
        let next = v["sequence_number"]
            .as_u64()
            .ok_or("managed Responses sequence missing")?;
        if sequence.is_some_and(|previous| next <= previous) {
            return Err("managed Responses sequence repeated".into());
        }
        sequence = Some(next);
        match kind {
            "response.completed" | "response.incomplete" => {
                let r = &v["response"];
                let usage_value = r.get("usage").filter(|u| !u.is_null()).ok_or_else(|| {
                    CompletionValidationFailure::usage(
                        "managed Responses usage missing",
                        ManagedProviderDiagnosticKind::UsageMissing,
                        ManagedUsageRejectionKind::MissingReceipt,
                    )
                })?;
                let snapshot = observe(usage_value);
                *observation = Some(snapshot.clone());
                let usage = parse_usage(usage_value, cap).map_err(|f| {
                    let mut failure = CompletionValidationFailure::from(f);
                    failure.responses_observation = Some(Box::new(snapshot.clone()));
                    failure
                })?;
                let incomplete = kind == "response.incomplete";
                if r["object"] != "response"
                    || r["model"].as_str() != Some(model)
                    || r["status"]
                        != if incomplete {
                            "incomplete"
                        } else {
                            "completed"
                        }
                    || r.get("error").is_some_and(|e| !e.is_null())
                    || r.get("max_output_tokens")
                        .is_some_and(|n| !n.is_null() && n.as_u64() != Some(u64::from(cap)))
                    || (incomplete && r["incomplete_details"]["reason"] != "max_output_tokens")
                    || (!incomplete && r.get("incomplete_details").is_some_and(|d| !d.is_null()))
                {
                    return Err("managed Responses terminal condition is unsupported".into());
                }
                let items = r["output"]
                    .as_array()
                    .ok_or("managed Responses output missing")?;
                let mut calls = BTreeSet::new();
                let mut reasoning = BTreeMap::new();
                for item in items {
                    match item["type"].as_str() {
                        Some("function_call") if !incomplete => {
                            let id = item["call_id"]
                                .as_str()
                                .filter(|id| !id.is_empty() && id.len() <= 128)
                                .ok_or("managed Responses call id unsupported")?;
                            let item_id = item["id"]
                                .as_str()
                                .ok_or("managed Responses item id missing")?;
                            if !items_done.contains(item_id)
                                || streamed_tools.get(item_id) != Some(item)
                                || arguments.get(item_id).map(String::as_str)
                                    != item["arguments"].as_str()
                            {
                                return Err(
                                    "managed Responses tool stream disagrees with receipt".into()
                                );
                            }
                            if !calls.insert(id)
                                || !safe_file_call(
                                    item["name"].as_str().unwrap_or_default(),
                                    item["arguments"].as_str().unwrap_or_default(),
                                )
                            {
                                return Err("managed Responses tool call unsupported".into());
                            }
                        }
                        Some("reasoning") => {
                            let (id, digest) = reasoning_key(item)?;
                            if reasoning.insert(id, digest).is_some()
                                || reasoning.len() > MAX_TOOL_CALLS as usize
                            {
                                return Err(
                                    "managed Responses reasoning context repeats or exceeds bounds"
                                        .into(),
                                );
                            }
                        }
                        Some("message") => {}
                        _ => {
                            return Err("managed Responses provider-side tool is unsupported".into())
                        }
                    }
                }
                if calls.len() != streamed_tools.len()
                    || calls.len() > MAX_TOOL_CALLS as usize
                    || streamed_tools.len() > MAX_TOOL_CALLS as usize
                {
                    return Err("managed Responses tool count exceeds bounds".into());
                }
                terminal = Some((calls.len() as u32, usage, snapshot, incomplete, reasoning));
            }
            "response.output_item.added" | "response.output_item.done" => {
                match v["item"]["type"].as_str() {
                    Some("function_call") => {
                        if !file_tool(v["item"]["name"].as_str().unwrap_or_default()) {
                            return Err("managed Responses streamed tool unsupported".into());
                        }
                        let id = v["item"]["id"]
                            .as_str()
                            .filter(|id| id.len() <= 128)
                            .ok_or("managed Responses tool id missing")?;
                        if kind == "response.output_item.added" {
                            if streamed_tools
                                .insert(id.to_owned(), v["item"].clone())
                                .is_some()
                            {
                                return Err(
                                    "managed Responses repeated or populated tool start".into()
                                );
                            }
                            let initial = v["item"]["arguments"]
                                .as_str()
                                .ok_or("managed Responses initial arguments malformed")?;
                            if !initial.is_empty()
                                && !safe_file_call(
                                    v["item"]["name"].as_str().unwrap_or_default(),
                                    initial,
                                )
                            {
                                return Err("managed Responses whole call unsupported".into());
                            }
                            arguments.insert(id.to_owned(), initial.to_owned());
                        } else {
                            if !items_done.insert(id.to_owned()) {
                                return Err("managed Responses tool done repeated".into());
                            }
                            let added = streamed_tools
                                .get(id)
                                .ok_or("managed Responses tool done without start")?;
                            if added["name"] != v["item"]["name"]
                                || added["call_id"] != v["item"]["call_id"]
                                || arguments.get(id).map(String::as_str)
                                    != v["item"]["arguments"].as_str()
                                || !safe_file_call(
                                    v["item"]["name"].as_str().unwrap_or_default(),
                                    v["item"]["arguments"].as_str().unwrap_or_default(),
                                )
                            {
                                return Err("managed Responses completed tool disagrees".into());
                            }
                            streamed_tools.insert(id.to_owned(), v["item"].clone());
                        }
                        if streamed_tools.len() > MAX_TOOL_CALLS as usize {
                            return Err(
                                "managed Responses streamed tool count exceeds bounds".into()
                            );
                        }
                    }
                    Some("message" | "reasoning") => {}
                    _ => return Err("managed Responses streamed provider tool unsupported".into()),
                }
            }
            "response.function_call_arguments.delta" | "response.function_call_arguments.done" => {
                let id = v["item_id"]
                    .as_str()
                    .ok_or("managed Responses arguments id missing")?;
                let accumulated = arguments
                    .get_mut(id)
                    .ok_or("managed Responses arguments without tool")?;
                if arguments_done.contains(id) || items_done.contains(id) {
                    return Err("managed Responses arguments continued after done".into());
                }
                if kind.ends_with(".delta") {
                    let initial = streamed_tools[id]["arguments"].as_str().unwrap_or_default();
                    if !initial.is_empty() {
                        if v["delta"].as_str() != Some(initial)
                            || !whole_call_delta.insert(id.to_owned())
                        {
                            return Err("managed Responses whole-call delta disagrees".into());
                        }
                    } else {
                        accumulated.push_str(
                            v["delta"]
                                .as_str()
                                .ok_or("managed Responses arguments delta malformed")?,
                        );
                    }
                } else if v["arguments"].as_str() != Some(accumulated.as_str()) {
                    return Err("managed Responses arguments done disagrees".into());
                }
                if kind.ends_with(".done") {
                    arguments_done.insert(id.to_owned());
                }
                if accumulated.len() > 16 * 1024 {
                    return Err("managed Responses arguments exceed bounds".into());
                }
            }
            "response.created" | "response.in_progress" => {
                let r = &v["response"];
                if r["model"].as_str() != Some(model)
                    || r["object"] != "response"
                    || r["status"] != "in_progress"
                    || !r["output"].as_array().is_some_and(Vec::is_empty)
                    || r.get("usage").is_some_and(|u| !u.is_null())
                {
                    return Err("managed Responses preliminary envelope unsupported".into());
                }
            }
            "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.delta"
            | "response.output_text.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done" => {}
            _ => return Err("managed Responses event unsupported".into()),
        }
    }
    terminal.ok_or_else(|| "managed Responses stream lacks terminal receipt".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn request(turn: u32) -> Value {
        json!({"model":"grok-build-0.1","input":[{"role":"user","content":format!("offline {turn}")}],"stream":true,"store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":1024,"reasoning":{"summary":"concise"},"tools":[{"type":"function","name":"write","parameters":{"type":"object"}}]})
    }
    fn usage() -> Value {
        json!({"input_tokens":100,"output_tokens":15,"total_tokens":115,"input_tokens_details":{"cached_tokens":20},"output_tokens_details":{"reasoning_tokens":5},"cost_in_usd_ticks":777})
    }
    fn stream(usage: Value, incomplete: bool) -> String {
        super::super::offline_tests::responses_turn(0, false, usage, incomplete)
    }

    #[test]
    fn responses_output_includes_reasoning_without_double_charge() {
        for (out, reasoning, cached, ticks) in [
            (10, 0, 0, None),
            (15, 5, 0, Some(777)),
            (15, 5, 20, Some(777)),
            (1024, 1024, 100, Some(0)),
        ] {
            let mut u = usage();
            u["output_tokens"] = json!(out);
            u["total_tokens"] = json!(100 + out);
            u["output_tokens_details"]["reasoning_tokens"] = json!(reasoning);
            u["input_tokens_details"]["cached_tokens"] = json!(cached);
            if let Some(t) = ticks {
                u["cost_in_usd_ticks"] = json!(t);
            } else {
                u.as_object_mut().unwrap().remove("cost_in_usd_ticks");
            }
            let parsed = parse_usage(&u, 1024).unwrap();
            assert_eq!(parsed.total_tokens, 100 + out);
            assert_eq!(parsed.reasoning_tokens, reasoning);
            assert_eq!(parsed.cost_in_usd_ticks, ticks);
        }
    }

    #[test]
    fn responses_opaque_reasoning_is_owned_unique_and_conservatively_reserved() {
        let item =
            json!({"type":"reasoning","id":"owned","summary":[],"encrypted_content":"opaque"});
        let (id, digest) = reasoning_key(&item).unwrap();
        let owned = BTreeMap::from([(id, digest)]);
        let mut body = request(0);
        body["input"] = json!([item]);
        assert_eq!(reasoning_reuse_allowance(&body, &owned, 1024), Ok(1024));
        assert!(reasoning_reuse_allowance(&body, &BTreeMap::new(), 1024).is_err());
        let item = body["input"][0].clone();
        body["input"] = json!([item, item]);
        assert!(reasoning_reuse_allowance(&body, &owned, 1024).is_err());
        body["input"] = json!([item]);
        body["input"][0]["encrypted_content"] = json!("changed");
        assert!(reasoning_reuse_allowance(&body, &owned, 1024).is_err());
    }

    #[test]
    fn responses_unknown_contradictory_and_overflow_receipts_fail_closed() {
        for (pointer, value) in [
            ("/total_tokens", json!(116)),
            ("/output_tokens_details/reasoning_tokens", json!(16)),
            ("/input_tokens_details/cached_tokens", json!(101)),
            ("/output_tokens", json!(1025)),
            ("/input_tokens", json!(u64::MAX)),
            ("/cost_in_usd_ticks", json!(-1)),
            ("/cost_in_usd_ticks", json!("777")),
            ("/cost_in_usd_ticks", Value::Null),
        ] {
            let mut u = usage();
            *u.pointer_mut(pointer).unwrap() = value;
            assert!(parse_usage(&u, 1024).is_err(), "{pointer}");
        }
        for key in ["unexplained_charge_ticks", "reasoning_tokens"] {
            let mut u = usage();
            u[key] = json!(1);
            assert!(parse_usage(&u, 1024).is_err());
        }
        let mut u = usage();
        u["output_tokens_details"]["unknown"] = json!(0);
        assert!(parse_usage(&u, 1024).is_err());
        assert!(parse_usage(&usage(), 0).is_err());
        assert!(parse_usage(&usage(), 1025).is_err());
    }

    #[test]
    fn responses_protocol_rejects_repeated_malformed_tools_and_usage() {
        let valid = stream(usage(), false);
        assert!(validate_completion(
            valid.as_bytes(),
            "child-secret",
            "upstream-secret",
            1024,
            "grok-build-0.1"
        )
        .is_ok());
        for invalid in [
            valid.repeat(2),
            valid.replace("response.completed", "response.failed"),
            valid.replace("\"sequence_number\":1", "\"sequence_number\":0"),
            valid.replace("\"usage\":{", "\"unknown_usage\":{"),
            "data: malformed\n\n".into(),
        ] {
            assert!(validate_completion(
                invalid.as_bytes(),
                "child-secret",
                "upstream-secret",
                1024,
                "grok-build-0.1"
            )
            .is_err());
        }
        let tools = super::super::offline_tests::responses_turn(0, true, usage(), false);
        assert_eq!(
            validate_completion(
                tools.as_bytes(),
                "child-secret",
                "upstream-secret",
                1024,
                "grok-build-0.1"
            )
            .unwrap()
            .0,
            2
        );
        let mut events = tools
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str::<Value>(data).unwrap())
            .collect::<Vec<_>>();
        let final_items = events.last().unwrap()["response"]["output"].clone();
        for event in &mut events {
            if event["type"] == "response.output_item.added"
                && event["item"]["type"] == "function_call"
            {
                let item = final_items
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|i| i["id"] == event["item"]["id"])
                    .unwrap();
                event["item"]["arguments"] = item["arguments"].clone();
            }
        }
        let whole = events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect::<String>();
        assert_eq!(
            validate_completion(
                whole.as_bytes(),
                "child-secret",
                "upstream-secret",
                1024,
                "grok-build-0.1"
            )
            .unwrap()
            .0,
            2
        );
        for invalid in [
            tools.replace("\"name\":\"write\"", "\"name\":\"terminal\""),
            tools.replacen("src/framing.py", "other.py", 1),
        ] {
            assert!(validate_completion(
                invalid.as_bytes(),
                "child-secret",
                "upstream-secret",
                1024,
                "grok-build-0.1"
            )
            .is_err());
        }
        assert!(!safe_file_call("use_tool", r#"{"tool_name":"terminal"}"#));
        assert!(!safe_file_call("use_tool", r#"{"tool_name":"use_tool"}"#));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn responses_sealed_authority_refuses_child_disagreement_before_wire() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::set_grokptah_home_override(None);
            }
        }
        let _reset = Reset;
        let count = Arc::new(AtomicU32::new(0));
        let calls = count.clone();
        let router = Router::new().route(
            "/v1/responses",
            post(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { stream(usage(), false) }
            }),
        );
        let (relay, server) = super::super::tests::local_fixture_with_backend(
            root.path().join("leases"),
            router,
            ManagedBackend::Responses,
        )
        .await;
        for (i, mutation) in [
            "omit",
            "raise",
            "wrong_model",
            "legacy_max",
            "legacy_completion",
            "server_tool",
            "reasoning",
            "store",
            "image_input",
            "remote_input",
        ]
        .into_iter()
        .enumerate()
        {
            let id = relay
                .lease_id_for_request("offline", &format!("request-{i}"))
                .unwrap();
            relay.resolve(&id).unwrap();
            let mut body = request(i as u32);
            match mutation {
                "omit" => {
                    body.as_object_mut().unwrap().remove("max_output_tokens");
                }
                "raise" => body["max_output_tokens"] = json!(1025),
                "wrong_model" => body["model"] = json!("grok-4.6"),
                "legacy_max" => body["max_tokens"] = json!(1024),
                "legacy_completion" => body["max_completion_tokens"] = json!(1024),
                "server_tool" => body["tools"] = json!([{"type":"web_search"}]),
                "reasoning" => body["reasoning"] = json!({"effort":"high"}),
                "store" => body["store"] = json!(true),
                "image_input" => {
                    body["input"] = json!([{"type":"message","role":"user","content":[{"type":"input_image","image_url":"https://unrelated.invalid/image"}]}])
                }
                "remote_input" => {
                    body["input"] =
                        json!([{"type":"remote_content","url":"https://unrelated.invalid"}])
                }
                _ => unreachable!(),
            }
            assert!(
                forward(&relay.state, &id, &serde_json::to_vec(&body).unwrap())
                    .await
                    .is_err()
            );
            assert_eq!(relay.evidence(&id).unwrap().wire_attempts, 0);
        }
        // Wrong paths and the pinned child's title model cannot consume or
        // revoke the intended assignment's bearer capability.
        let id = relay.lease_id_for_request("offline", "auxiliary").unwrap();
        relay.resolve(&id).unwrap();
        let capability = relay.state.leases.lock().unwrap()[&id].secret.clone();
        let client = reqwest::Client::new();
        for path in ["chat/completions", "messages"] {
            let response = client
                .post(format!("{}/{path}", relay.policy().endpoint))
                .bearer_auth(capability.trim())
                .json(&request(99))
                // authority-allow-unauthenticated-wire: Test-only local relay
                // capability; all upstream sends retain canonical authority.
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        let mut auxiliary = request(99);
        auxiliary["model"] = json!("grok-4.6");
        assert_eq!(
            client
                .post(format!("{}/responses", relay.policy().endpoint))
                .bearer_auth(capability.trim())
                .json(&auxiliary)
                // authority-allow-unauthenticated-wire: Test-only local relay
                // capability; all upstream sends retain canonical authority.
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let evidence = relay.evidence(&id).unwrap();
        assert_eq!(evidence.requests_reserved, 0);
        assert!(!evidence.revoked);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn responses_incomplete_is_settled_bounded_and_terminal() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::set_grokptah_home_override(None);
            }
        }
        let _reset = Reset;
        let count = Arc::new(AtomicU32::new(0));
        let calls = count.clone();
        let mut u = usage();
        u["output_tokens"] = json!(1024);
        u["total_tokens"] = json!(1124);
        u["output_tokens_details"]["reasoning_tokens"] = json!(1024);
        u["cost_in_usd_ticks"] = json!(0);
        let payload = stream(u, true);
        let router = Router::new().route(
            "/v1/responses",
            post(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                let p = payload.clone();
                async { p }
            }),
        );
        let (relay, server) = super::super::tests::local_fixture_with_backend(
            root.path().join("leases"),
            router,
            ManagedBackend::Responses,
        )
        .await;
        let id = relay.lease_id_for_request("offline", "incomplete").unwrap();
        relay.resolve(&id).unwrap();
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&request(0)).unwrap())
                .await
                .is_ok()
        );
        let e = relay.evidence(&id).unwrap();
        assert!(e.revoked && e.accounting_complete && !e.uncertain);
        assert_eq!(e.total_tokens, Some(1124));
        assert_eq!(e.reasoning_tokens, 1024);
        assert_eq!(e.cost_in_usd_ticks, Some(0));
        assert_eq!(
            e.interruption,
            Some(ManagedProviderDiagnosticKind::OutputLimitIncomplete)
        );
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&request(1)).unwrap())
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn responses_reserve_uses_settled_total_and_cache_never_creates_authority() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::set_grokptah_home_override(None);
            }
        }
        let _reset = Reset;
        let count = Arc::new(AtomicU32::new(0));
        let calls = count.clone();
        let router = Router::new().route(
            "/v1/responses",
            post(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { stream(usage(), false) }
            }),
        );
        let (relay, server) = super::super::tests::local_fixture_with_backend(
            root.path().join("leases"),
            router,
            ManagedBackend::Responses,
        )
        .await;
        let mut sealed = request(0);
        sealed["parallel_tool_calls"] = json!(false);
        let reserve = serde_json::to_vec(&sealed).unwrap().len() as u64 + 1024;
        let id = relay.lease_id_for_request("offline", "reserve").unwrap();
        relay
            .bind_lease_bounds(&id, 6, 180000, reserve + 114)
            .unwrap();
        relay.resolve(&id).unwrap();
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&request(0)).unwrap())
                .await
                .is_ok()
        );
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&request(1)).unwrap())
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(relay
            .evidence(&id)
            .unwrap()
            .diagnostics
            .iter()
            .any(|d| d.admission_denial == Some(ManagedAdmissionDenial::TokenReserve)));
        let id = relay.lease_id_for_request("offline", "overflow").unwrap();
        relay.resolve(&id).unwrap();
        {
            let mut leases = relay.state.leases.lock().unwrap();
            let e = &mut leases.get_mut(&id).unwrap().evidence;
            e.input_tokens = u64::MAX;
            e.output_tokens = 1;
            e.total_tokens = Some(u64::MAX);
            e.usage_observed = true;
        }
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&request(0)).unwrap())
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn responses_reused_reasoning_cannot_spend_unreserved_latent_input() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::set_grokptah_home_override(None);
            }
        }
        let _reset = Reset;
        let count = Arc::new(AtomicU32::new(0));
        let calls = count.clone();
        let router = Router::new().route(
            "/v1/responses",
            post(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { super::super::offline_tests::responses_turn(0, true, usage(), false) }
            }),
        );
        let (relay, server) = super::super::tests::local_fixture_with_backend(
            root.path().join("leases"),
            router,
            ManagedBackend::Responses,
        )
        .await;
        let item = json!({"type":"reasoning","id":"reason_0","summary":[{"type":"summary_text","text":"Synthetic bounded file reasoning"}],"encrypted_content":"synthetic-offline-reasoning"});
        let mut next = request(1);
        next["input"].as_array_mut().unwrap().push(item);
        next["parallel_tool_calls"] = json!(false);
        let reservation = serde_json::to_vec(&next).unwrap().len() as u64 + 1024;
        let id = relay
            .lease_id_for_request("offline", "latent-reserve")
            .unwrap();
        relay
            .bind_lease_bounds(&id, 6, 180000, reservation + 115 + 14)
            .unwrap();
        relay.resolve(&id).unwrap();
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&request(0)).unwrap())
                .await
                .is_ok()
        );
        assert_eq!(relay.state.leases.lock().unwrap()[&id].reasoning.len(), 1);
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&next).unwrap())
                .await
                .is_err()
        );
        let e = relay.evidence(&id).unwrap();
        assert_eq!(e.total_tokens, Some(115));
        assert_eq!(e.output_tokens, 15);
        assert!(e
            .diagnostics
            .iter()
            .any(|d| d.admission_denial == Some(ManagedAdmissionDenial::TokenReserve)));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
