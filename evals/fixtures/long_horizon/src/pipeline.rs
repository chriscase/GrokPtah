use crate::emitter::emit;
use crate::parser::parse;

pub fn run(input: &str) -> String {
    emit(&parse(input))
}
