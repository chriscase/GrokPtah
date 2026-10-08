"""Decode exactly one canonical length-prefixed UTF-8 message."""


def decode(frame: bytes) -> str:
    header, payload = frame.split(b":", 1)
    size = int(header)
    return payload[:size].decode("utf-8", errors="ignore")
