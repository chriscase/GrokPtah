/// Clamp into 0..=255. BUG: uses modulo (wrong for negatives / overflow).
pub fn clamp_u8(x: i32) -> u8 {
    (x % 256) as u8
}
