"""Validate the published synthetic, sanitized pinned-CLI capture."""

import json
from pathlib import Path

path = Path(__file__).with_name("pinned-cli-1.0.41-capture-20260928.jsonl")
records = [json.loads(line) for line in path.read_text().splitlines()]
expected = [
    ("GET", "/v1/models"),
    ("GET", "/v1/settings"),
    ("GET", "/v1/settings"),
    ("GET", "/v1/bundle/archive"),
    ("GET", "/v1/subagents/bundle"),
    ("GET", "/"),
    ("POST", "/v1/responses"),
    ("POST", "/v1/chat/completions"),
]
assert [(r["method"], r["path"]) for r in records] == expected
for record in records:
    assert "authorization" not in record["safe_headers"]
    assert "x-userid" not in record["safe_headers"]
    assert "x-grok-user-id" not in record["safe_headers"]
    assert "x-email" not in record["safe_headers"]
    assert "x-teamid" not in record["safe_headers"]
    assert set(record) == {
        "method", "path", "header_names", "safe_headers",
        "authorization_present", "body_keys", "body_subset",
    }
turn = records[-1]
assert turn["authorization_present"]
assert turn["body_subset"] == {
    "model": "grok-build-0.1", "stream": True,
    "stream_options": {"include_usage": True},
}
assert turn["safe_headers"]["x-grok-model-override"] == turn["body_subset"]["model"]
assert turn["safe_headers"]["x-xai-token-auth"] == "xai-grok-cli"
assert turn["safe_headers"]["x-authenticateresponse"] == "authenticate-response"
assert turn["safe_headers"]["x-grok-client-version"] == "1.0.41"
assert turn["safe_headers"]["x-grok-client-mode"] == "headless"
assert records[-2]["body_subset"]["model"] == "grok-4.6"
print("sanitized pinned CLI capture: 8 expected local requests, model override bound")
