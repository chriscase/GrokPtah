"""Host-owned behavioral oracle, copied outside the worker's writable scope."""

import os
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(os.environ["CANDIDATE_ROOT"]) / "src"))
from framing import encode
from decoder import decode


class FrameContract(unittest.TestCase):
    def test_roundtrip_and_byte_length(self):
        for text in ("", "plain ASCII", "colon:inside", "café", "🐱 café é"):
            with self.subTest(text=text):
                payload = text.encode("utf-8")
                expected = str(len(payload)).encode("ascii") + b":" + payload
                self.assertEqual(encode(text), expected)
                self.assertEqual(decode(expected), text)

    def test_canonical_unsigned_ascii_header(self):
        for frame in (b":", b"00:", b"01:a", b"-1:a", b"+1:a", b" 1:a", b"1 :a", b"x:a", b"1_0:abcdefghij", b"\x80:a"):
            with self.subTest(frame=frame), self.assertRaises(ValueError):
                decode(frame)

    def test_exact_payload_size(self):
        for frame in (b"2:a", b"1:ab", b"0:x", b"9:", b"2:\xc3", b"no separator"):
            with self.subTest(frame=frame), self.assertRaises(ValueError):
                decode(frame)

    def test_strict_utf8(self):
        for frame in (b"1:\xff", b"1:\xc3", b"2:\xed\xa0", b"3:\xed\xa0\x80"):
            with self.subTest(frame=frame), self.assertRaises(ValueError):
                decode(frame)


if __name__ == "__main__":
    unittest.main(verbosity=2)
