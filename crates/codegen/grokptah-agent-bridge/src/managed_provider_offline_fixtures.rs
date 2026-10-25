//! Portable offline Responses wire fixtures for unit tests on every host.
//! macOS installed-CLI qualification lives in `managed_provider_offline_tests.rs`.

use super::json;
use super::Value;

pub const FIXED_FRAMING: &str = "def encode(message: str) -> bytes:\n    payload = message.encode('utf-8')\n    return str(len(payload)).encode('ascii') + b':' + payload\n";
pub const FIXED_DECODER: &str = "def decode(frame: bytes) -> str:\n    header, separator, payload = frame.partition(b':')\n    if not separator or header != str(len(payload)).encode('ascii'):\n        raise ValueError('invalid frame')\n    return payload.decode('utf-8')\n";

pub(super) fn responses_turn(index: u32, tools: bool, usage: Value, incomplete: bool) -> String {
    let mut items = if tools {
        [("src/framing.py", FIXED_FRAMING), ("src/decoder.py", FIXED_DECODER)]
            .into_iter()
            .enumerate()
            .map(|(i, (path, code))| {
                json!({"type":"function_call","id":format!("fc_{index}_{i}"),"call_id":format!("call_{index}_{i}"),"name":"write","arguments":json!({"file_path":path,"content":code}).to_string(),"status":"completed"})
            })
            .collect::<Vec<_>>()
    } else {
        vec![
            json!({"type":"message","id":format!("msg_{index}"),"role":"assistant","status":"completed","content":[{"type":"output_text","text":"Repaired both files.\nGROK_BUILD_VERDICT=clean","annotations":[],"logprobs":[]}]}),
        ]
    };
    if tools
        && usage["output_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or(0)
            > 0
    {
        items.push(json!({"type":"reasoning","id":format!("reason_{index}"),"summary":[{"type":"summary_text","text":"Synthetic bounded file reasoning"}],"encrypted_content":"synthetic-offline-reasoning","status":"completed"}));
    }
    let response = json!({"id":format!("resp_{index}"),"object":"response","created_at":0,"model":"grok-build-0.1","status":if incomplete {"incomplete"} else {"completed"},"max_output_tokens":1024,"output":items,"usage":usage,"error":null,"incomplete_details":if incomplete {json!({"reason":"max_output_tokens"})} else {Value::Null}});
    let mut events = Vec::new();
    let mut created = response.clone();
    created["output"] = json!([]);
    created["usage"] = Value::Null;
    created["status"] = json!("in_progress");
    created["incomplete_details"] = Value::Null;
    events.push(json!({"type":"response.created","response":created}));
    for (i, item) in items.iter().enumerate() {
        let mut added = item.clone();
        added["status"] = json!("in_progress");
        if item["type"] == "function_call" {
            added["arguments"] = json!("");
        }
        events.push(json!({"type":"response.output_item.added","output_index":i,"item":added}));
        if item["type"] == "function_call" {
            events.push(json!({"type":"response.function_call_arguments.delta","output_index":i,"item_id":item["id"],"delta":item["arguments"]}));
            events.push(json!({"type":"response.function_call_arguments.done","output_index":i,"item_id":item["id"],"arguments":item["arguments"],"name":item["name"]}));
        } else if item["type"] == "message" {
            events.push(json!({"type":"response.output_text.delta","output_index":i,"item_id":item["id"],"content_index":0,"delta":item["content"][0]["text"],"logprobs":[]}));
        }
        events.push(json!({"type":"response.output_item.done","output_index":i,"item":item}));
    }
    events.push(json!({"type":if incomplete {"response.incomplete"} else {"response.completed"},"response":response}));
    events
        .into_iter()
        .enumerate()
        .map(|(i, mut v)| {
            v["sequence_number"] = json!(i);
            format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap())
        })
        .collect()
}
