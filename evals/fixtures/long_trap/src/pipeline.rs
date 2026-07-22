use crate::{emitter, parser};

pub fn run(raw: &str) -> String {
    let p = parser::parse(raw);
    emitter::emit(p)
}
