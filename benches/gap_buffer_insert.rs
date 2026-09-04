//! Task 13 Step 4: GapBuffer 插入性能基准
//!
//! 使用 std::time::Instant 测量（避免引入 criterion 依赖）。
//! 运行: cargo bench -q

use std::time::Instant;
use beditor::block::GapBuffer;

fn bench<F: Fn()>(name: &str, iterations: usize, f: F) {
    // 预热
    f();
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = start.elapsed();
    let per_op = elapsed / iterations as u32;
    println!("{name:>40} | {iterations:>8} ops | {elapsed:>10?} | {per_op:>8?}/op");
}

fn main() {
    println!("=== GapBuffer Insert Benchmark ===\n");

    // 1. 顺序插入 1KB 到空 buffer
    bench("insert_1KB_sequential", 1000, || {
        let mut gb = GapBuffer::new(vec![]);
        let chunk = vec![0x41u8; 1024];
        gb.insert(0, &chunk).unwrap();
    });

    // 2. 在 256KB buffer 中间插入 1 字节（触发 gap 移动）
    bench("insert_1byte_mid_256KB", 1000, || {
        let init = vec![0x41u8; 256 * 1024];
        let mut gb = GapBuffer::new(init);
        gb.insert(128 * 1024, b"X").unwrap();
    });

    // 3. 在 256KB buffer 末尾插入 1KB（无需移动 gap）
    bench("insert_1KB_at_end_256KB", 1000, || {
        let init = vec![0x41u8; 256 * 1024];
        let mut gb = GapBuffer::new(init);
        let chunk = vec![0x42u8; 1024];
        let pos = 256 * 1024;
        gb.insert(pos, &chunk).unwrap();
    });

    // 4. 多次小块插入（模拟用户逐字输入）
    bench("insert_100x_1byte", 500, || {
        let mut gb = GapBuffer::new(vec![]);
        for i in 0..100usize {
            gb.insert(i, b"X").unwrap();
        }
    });

    // 5. 大块插入（4KB）到 1MB buffer 中间
    bench("insert_4KB_mid_1MB", 100, || {
        let init = vec![0x41u8; 1024 * 1024];
        let mut gb = GapBuffer::new(init);
        let chunk = vec![0x42u8; 4096];
        gb.insert(512 * 1024, &chunk).unwrap();
    });

    println!("\n=== GapBuffer Delete Benchmark ===\n");

    // 6. 删除 1KB 从 256KB buffer 中间
    bench("delete_1KB_mid_256KB", 1000, || {
        let init = vec![0x41u8; 256 * 1024];
        let mut gb = GapBuffer::new(init);
        gb.delete(128 * 1024, 1024).unwrap();
    });

    // 7. as_contiguous 从 256KB GapBuffer
    bench("as_contiguous_256KB", 1000, || {
        let init = vec![0x41u8; 256 * 1024];
        let mut gb = GapBuffer::new(init);
        // 插入后产生 gap
        gb.insert(0, b"X").unwrap();
        let _ = gb.as_contiguous();
    });
}
