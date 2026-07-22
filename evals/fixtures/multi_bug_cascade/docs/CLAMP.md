# CLAMP

`clamp_u8(x)` should clamp into 0..=255 inclusive.
Currently returns x % 256 which is wrong for negatives.
