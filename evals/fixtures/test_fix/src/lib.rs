/// Should return the larger of a and b.
pub fn max_i32(a: i32, b: i32) -> i32 {
    // BUG: returns smaller
    if a < b { a } else { b }
}
