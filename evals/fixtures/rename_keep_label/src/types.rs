#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldThing {
    pub id: u32,
}

impl OldThing {
    pub fn new(id: u32) -> Self {
        Self { id }
    }
}
