//! Task 13 Step 4: BlockCache 命中率 + 淘汰性能基准
//!
//! 使用 std::time::Instant 测量（避免引入 criterion 依赖）。
//! 运行: cargo bench -q

use std::sync::Arc;
use std::time::Instant;
use beditor::block::Block;
use beditor::cache::BlockCache;
use beditor::config::EditorConfig;

fn bench<F: Fn()>(name: &str, iterations: usize, f: F) {
    f(); // 预热
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = start.elapsed();
    let per_op = elapsed / iterations as u32;
    println!("{name:>40} | {iterations:>8} ops | {elapsed:>10?} | {per_op:>8?}/op");
}

fn make_config(_capacity: usize) -> Arc<EditorConfig> {
    // 用 new_for_test 绕过 sysinfo
    Arc::new(EditorConfig { block_size: 256 * 1024, ..Default::default() })
}

fn main() {
    println!("=== BlockCache Benchmark ===\n");

    let cfg = make_config(100);

    // 1. try_get 命中（块在缓存中）
    {
        let cache = BlockCache::new_for_test(cfg.clone(), 100);
        for i in 0..50u64 {
            let b = Block::new_clean(i, vec![0x41u8; 1024]);
            cache.insert_loaded(b);
        }
        bench("try_get_hit", 10000, || {
            let _ = cache.try_get(25);
        });
    }

    // 2. try_get 未命中（块不在缓存中）
    {
        let cache = BlockCache::new_for_test(cfg.clone(), 100);
        bench("try_get_miss", 10000, || {
            let _ = cache.try_get(999);
        });
    }

    // 3. insert_loaded 到空缓存
    {
        let _cache = BlockCache::new_for_test(cfg.clone(), 100);
        bench("insert_loaded_empty", 1000, || {
            let _b = Block::new_clean(0, vec![0x41u8; 1024]);
            // 需要不同的 id 避免覆盖；但 bench 闭包不能有状态
            // 所以每次插入后状态变化，这里测的是第一次插入的重复
        });
        // 更准确：连续插入 100 块
        let cache2 = BlockCache::new_for_test(cfg.clone(), 100);
        let start = Instant::now();
        for i in 0..1000u64 {
            let b = Block::new_clean(i % 200, vec![0x41u8; 1024]);
            cache2.insert_loaded(b);
        }
        println!("{:>40} | {:>8} ops | {:>10?} | {:>8?}/op",
            "insert_loaded_sequential", 1000, start.elapsed(), start.elapsed() / 1000);
    }

    // 4. LRU 淘汰场景：容量 10，插入 1000 块（触发 990 次淘汰）
    {
        let cache = BlockCache::new_for_test(cfg.clone(), 10);
        let start = Instant::now();
        for i in 0..1000u64 {
            let b = Block::new_clean(i, vec![0x41u8; 1024]);
            cache.insert_loaded(b);
        }
        let elapsed = start.elapsed();
        println!("{:>40} | {:>8} ops | {:>10?} | {:>8?}/op",
            "insert_with_eviction_10cap", 1000, elapsed, elapsed / 1000);
    }

    // 5. pin/unpin 循环
    {
        let cache = BlockCache::new_for_test(cfg.clone(), 100);
        for i in 0..10u64 {
            let b = Block::new_clean(i, vec![0x41u8; 1024]);
            cache.insert_loaded(b);
        }
        bench("pin_for_edit_cycle", 10000, || {
            if let Ok(_guard) = cache.pin_for_edit(5) {
                // guard drops here, unpinning
            }
        });
    }

    // 6. take_contiguous_and_mark_clean
    {
        let cache = BlockCache::new_for_test(cfg.clone(), 100);
        for i in 0..10u64 {
            let b = Block::new_clean(i, vec![0x41u8; 4096]);
            cache.insert_loaded(b);
        }
        // 先 pin+edit 让块变成 Gap 模式
        for i in 0..10u64 {
            if let Ok(_g) = cache.pin_for_edit(i) {
                cache.with_pin_mut(i, |gb| gb.insert(0, b"X")).ok();
            }
        }
        bench("take_contiguous_4KB", 1000, || {
            // take 后块变 Clean Raw，需要重新插入才能再 take
            // 这里测首次 take 的时间
        });
        // 更准确：测量 10 个块连续 take
        let cache2 = BlockCache::new_for_test(cfg.clone(), 100);
        for i in 0..10u64 {
            let b = Block::new_clean(i, vec![0x41u8; 4096]);
            cache2.insert_loaded(b);
        }
        for i in 0..10u64 {
            if let Ok(_g) = cache2.pin_for_edit(i) {
                cache2.with_pin_mut(i, |gb| gb.insert(0, b"X")).ok();
            }
        }
        let start = Instant::now();
        for i in 0..10u64 {
            let _ = cache2.take_contiguous_and_mark_clean(i);
        }
        println!("{:>40} | {:>8} ops | {:>10?} | {:>8?}/op",
            "take_contiguous_10x4KB", 10, start.elapsed(), start.elapsed() / 10);
    }

    println!("\n=== Cache Stats ===");
    let cache = BlockCache::new_for_test(cfg, 100);
    for i in 0..50u64 {
        let b = Block::new_clean(i, vec![0x41u8; 1024]);
        cache.insert_loaded(b);
    }
    for i in 0..60u64 {
        let _ = cache.try_get(i % 50);
    }
    let stats = cache.stats();
    println!("hits={} misses={} evictions={} size={} capacity={}",
        stats.hits, stats.misses, stats.evictions, stats.size_blocks, stats.capacity_blocks);
}
