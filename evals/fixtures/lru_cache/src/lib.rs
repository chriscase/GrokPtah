/// Fixed-capacity LRU cache. Stub — implement so tests pass.
pub struct LruCache {
    _cap: usize,
}

impl LruCache {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self { _cap: capacity }
    }

    pub fn get(&mut self, _key: i32) -> Option<i32> {
        None
    }

    pub fn put(&mut self, _key: i32, _value: i32) {}

    pub fn len(&self) -> usize {
        0
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
