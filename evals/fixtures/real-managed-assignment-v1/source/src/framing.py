"""Encode a UTF-8 message as ASCII byte-length, colon, payload."""


def encode(text: str) -> bytes:
    payload = text.encode("utf-8")
    return str(len(text)).encode("ascii") + b":" + payload
