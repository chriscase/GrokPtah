import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path('/private/tmp/rma-pinned-oidc-c3s1vy7b')
PORT = 49173
LOG = ROOT / 'capture.jsonl'
MODE = (ROOT / 'models-mode.txt').read_text().strip()
SAFE_HEADERS = {
    'user-agent', 'x-grok-client-version', 'x-grok-client-mode',
    'x-xai-token-auth', 'x-grok-model-override', 'x-grok-client-identifier',
    'x-authenticateresponse', 'content-type', 'accept',
}


def bounded(value):
    if isinstance(value, (str, int, float, bool)) or value is None:
        if isinstance(value, str):
            return value[:160]
        return value
    if isinstance(value, dict):
        return {k: bounded(v) for k, v in value.items() if k in {
            'include_usage', 'effort', 'type', 'max_output_tokens',
            'max_tokens', 'temperature', 'top_p', 'parallel_tool_calls',
        }}
    return '<redacted>'


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def capture_request(self):
        length = min(int(self.headers.get('Content-Length', 0)), 2_000_000)
        raw = self.rfile.read(length) if length else b''
        try:
            body = json.loads(raw) if raw else {}
        except (ValueError, UnicodeDecodeError):
            body = {}
        headers = {k.lower(): v for k, v in self.headers.items()}
        subset = {}
        if isinstance(body, dict):
            for key in ('model', 'stream', 'stream_options', 'reasoning', 'reasoning_effort',
                        'max_output_tokens', 'max_tokens', 'temperature', 'top_p',
                        'parallel_tool_calls'):
                if key in body:
                    subset[key] = bounded(body[key])
        record = {
            'method': self.command,
            'path': self.path,
            'header_names': sorted(headers),
            'safe_headers': {k: bounded(v) for k, v in headers.items() if k in SAFE_HEADERS},
            'authorization_present': 'authorization' in headers,
            'body_keys': sorted(body) if isinstance(body, dict) else [],
            'body_subset': subset,
        }
        with LOG.open('a') as f:
            f.write(json.dumps(record, sort_keys=True) + '\n')
        return body

    def send_json(self, status, value):
        data = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.capture_request()
        if self.path in ('/v1/models', '/v1/models-v2'):
            data = []
            if MODE == 'advertised-intended':
                data = [{'id': 'grok-build-0.1', 'object': 'model',
                         'created': 0, 'owned_by': 'xai'}]
            self.send_json(200, {'object': 'list', 'data': data})
        elif self.path == '/v1/settings':
            self.send_json(200, {})
        elif self.path == '/v1/login-config':
            self.send_json(200, {})
        else:
            self.send_json(404, {'error': 'unexpected fixture path'})

    def do_POST(self):
        self.capture_request()
        if self.path.endswith('/chat/completions'):
            payload = ('data: {"id":"synthetic","created":1,"model":"grok-build-0.1","object":"chat.completion.chunk",'
                       '"choices":[{"index":0,"delta":{"role":"assistant",'
                       '"content":"OFFLINE_OK"},"finish_reason":null}]}\n\n'
                       'data: {"id":"synthetic","created":1,"model":"grok-build-0.1","object":"chat.completion.chunk",'
                       '"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n'
                       'data: [DONE]\n\n').encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        elif self.path.endswith('/responses'):
            self.send_json(200, {'id': 'synthetic', 'object': 'response', 'status': 'completed',
                                 'output': [], 'usage': {'input_tokens': 1, 'output_tokens': 1,
                                                        'total_tokens': 2}})
        else:
            self.send_json(404, {'error': 'unexpected fixture path'})


ThreadingHTTPServer(('127.0.0.1', PORT), Handler).serve_forever()
