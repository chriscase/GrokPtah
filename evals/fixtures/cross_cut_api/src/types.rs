#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyWidget {
    pub id: u64,
    pub label: String,
}

impl LegacyWidget {
    pub fn new(id: u64, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
        }
    }
}
