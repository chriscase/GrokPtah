use lru_mini::LruCache;

#[test]
fn basic_put_get() {
    let mut c = LruCache::new(2);
    c.put(1, 10);
    c.put(2, 20);
    assert_eq!(c.get(1), Some(10));
    assert_eq!(c.get(2), Some(20));
    assert_eq!(c.len(), 2);
}

#[test]
fn eviction_order_lru() {
    let mut c = LruCache::new(2);
    c.put(1, 1);
    c.put(2, 2);
    // Access 1 so 2 becomes LRU
    assert_eq!(c.get(1), Some(1));
    c.put(3, 3); // should evict key 2
    assert_eq!(c.get(2), None);
    assert_eq!(c.get(1), Some(1));
    assert_eq!(c.get(3), Some(3));
}

#[test]
fn update_moves_to_recent() {
    let mut c = LruCache::new(2);
    c.put(1, 1);
    c.put(2, 2);
    c.put(1, 100); // update + touch
    c.put(3, 3); // evict 2
    assert_eq!(c.get(2), None);
    assert_eq!(c.get(1), Some(100));
}

#[test]
fn get_missing() {
    let mut c = LruCache::new(1);
    assert_eq!(c.get(9), None);
    c.put(9, 9);
    assert_eq!(c.get(9), Some(9));
    c.put(8, 8);
    assert_eq!(c.get(9), None);
}
