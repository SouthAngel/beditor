use std::sync::Arc;
use beditor::block::{Block, BlockId};
use beditor::cache::BlockCache;
use beditor::config::EditorConfig;

fn make_cfg() -> Arc<EditorConfig> {
    Arc::new(EditorConfig::default())
}

fn make_block(id: BlockId) -> Block {
    Block::new_clean(id, vec![(id & 0xFF) as u8; 100])
}

#[test]
fn basic_lru_eviction() {
    let cfg = make_cfg();
    let cache = BlockCache::new_for_test(cfg, 3);
    cache.insert_loaded(make_block(0));
    cache.insert_loaded(make_block(1));
    cache.insert_loaded(make_block(2));
    // 插入第 4 块 → 块 0 应被 evict（最久未访问）
    cache.insert_loaded(make_block(3));
    assert!(cache.try_get(0).is_none(), "块0应被淘汰");
    assert!(cache.try_get(1).is_some());
    assert!(cache.try_get(2).is_some());
    assert!(cache.try_get(3).is_some());
    let s = cache.stats();
    assert_eq!(s.evictions, 1);
    assert_eq!(s.size_blocks, 3);
}

#[test]
fn access_reorders_lru() {
    let cfg = make_cfg();
    let cache = BlockCache::new_for_test(cfg, 3);
    cache.insert_loaded(make_block(0));
    cache.insert_loaded(make_block(1));
    cache.insert_loaded(make_block(2));
    // 访问块 0 → 变最近；插入块 3 → 淘汰块 1
    cache.try_get(0);
    cache.insert_loaded(make_block(3));
    assert!(cache.try_get(0).is_some(), "块0刚访问过不应淘汰");
    assert!(cache.try_get(1).is_none(), "块1应被淘汰");
}

#[test]
fn pinned_blocks_not_evicted() {
    let cfg = make_cfg();
    let cache = BlockCache::new_for_test(cfg, 2);
    cache.insert_loaded(make_block(0));
    cache.insert_loaded(make_block(1));
    let guard = cache.pin_for_edit(0).expect("pin OK");
    // 插入块 2：只有块 1 能被淘汰
    cache.insert_loaded(make_block(2));
    assert!(cache.try_get(0).is_some(), "被pin的块0不可淘汰");
    assert!(cache.try_get(1).is_none());
    assert!(cache.try_get(2).is_some());
    drop(guard); // unpin
    // 再插入块 3 → 块 0 现在可被淘汰
    cache.insert_loaded(make_block(3));
    assert!(cache.try_get(0).is_none(), "unpin后块0可淘汰");
}

#[test]
fn hit_miss_stats() {
    let cfg = make_cfg();
    let cache = BlockCache::new_for_test(cfg, 4);
    cache.insert_loaded(make_block(0));
    cache.try_get(0);
    cache.try_get(0); // 2 hits
    cache.try_get(999); // 1 miss
    let s = cache.stats();
    assert_eq!(s.hits, 2);
    assert_eq!(s.misses, 1);
}
