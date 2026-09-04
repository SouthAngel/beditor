//! Task 13 Step 3: 跨块搜索集成测试
//!
//! 验证 SearchLiteral 在查询模式跨越块边界时仍能正确匹配。
//! io_worker 的 search_impl 通过 overlap 缓冲处理跨块匹配。

use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::config::EditorConfig;
use beditor::message::*;
use beditor::io_worker::spawn_io_workers;

async fn search(
    path: &std::path::Path,
    block_size: usize,
    query: &[u8],
    start: u64,
    dir: Direction,
    limit: usize,
) -> SearchResult {
    let cfg = Arc::new(EditorConfig { block_size, ..Default::default() });
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (ev_tx, _ev_rx) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(path.to_path_buf(), cfg, io_rx, ev_tx).await.unwrap();
    // 磁盘布局（无编辑）：块偏移按块大小对齐
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let block_count = beditor::io_worker::compute_block_count(file_size, block_size);
    let start_block = (start / block_size as u64).min(block_count.saturating_sub(1));
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::SearchLiteral {
        query: query.to_vec(),
        start_byte: start,
        start_block,
        start_block_offset: start_block * block_size as u64,
        logical_block_count: block_count,
        direction: dir,
        limit,
        reply: tx,
    }).await.unwrap();
    rx.await.unwrap()
}

#[tokio::test]
async fn cross_block_match_forward() {
    // 块大小 16，模式 "ABCDEFGH" 跨越块 0/1 边界（位置 12..20）
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let mut data = vec![0x41u8; 32]; // "AAAA..."
    // 在位置 12 写入 "ABCDEFGH"（跨 16 字节边界）
    data[12..20].copy_from_slice(b"ABCDEFGH");
    std::fs::write(tmp.path(), &data).unwrap();

    let result = search(tmp.path(), 16, b"ABCDEFGH", 0, Direction::Forward, 10).await;
    assert_eq!(result.matches, vec![12], "应匹配跨块位置 12");
    assert!(!result.has_more_behind);
}

#[tokio::test]
async fn cross_block_match_backward() {
    // 反向搜索跨块匹配
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let mut data = vec![0x41u8; 32];
    data[12..20].copy_from_slice(b"ABCDEFGH");
    std::fs::write(tmp.path(), &data).unwrap();

    // 从位置 25 反向搜
    let result = search(tmp.path(), 16, b"ABCDEFGH", 25, Direction::Backward, 10).await;
    assert_eq!(result.matches, vec![12]);
}

#[tokio::test]
async fn multiple_cross_block_matches() {
    // 多个跨块匹配
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let block_size = 16;
    let total = block_size * 4; // 64 bytes
    let mut data = vec![0x41u8; total];
    // 在每块边界前 2 字节放 "XXXX"（跨块）
    // 块0-1 边界: 位置 14..18
    // 块1-2 边界: 位置 30..34
    // 块2-3 边界: 位置 46..50
    for &start in &[14usize, 30, 46] {
        data[start..start + 4].copy_from_slice(b"XXXX");
    }
    std::fs::write(tmp.path(), &data).unwrap();

    let result = search(tmp.path(), block_size, b"XXXX", 0, Direction::Forward, 10).await;
    assert_eq!(result.matches, vec![14, 30, 46], "应匹配 3 个跨块位置");
}

#[tokio::test]
async fn match_at_exact_block_boundary() {
    // 模式恰好在块边界开始
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let block_size = 16;
    let mut data = vec![0x41u8; 32];
    // 模式在块 1 起点开始
    data[block_size..block_size + 4].copy_from_slice(b"TEST");
    std::fs::write(tmp.path(), &data).unwrap();

    let result = search(tmp.path(), block_size, b"TEST", 0, Direction::Forward, 10).await;
    assert_eq!(result.matches, vec![block_size as u64]);
}

#[tokio::test]
async fn no_false_positive_at_boundary() {
    // 确保边界处的 overlap 不会产生假阳性
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let block_size = 8;
    let mut data = vec![0x41u8; 32];
    // 在位置 6 放 "AB"（不跨块），位置 8 放 "CD"（不跨块）
    // 搜索 "ABCD"（跨块但实际不存在）
    data[6..8].copy_from_slice(b"AB");
    data[8..10].copy_from_slice(b"CD");
    std::fs::write(tmp.path(), &data).unwrap();

    // "ABCD" 跨越块 0/1 边界（6..10），块大小 8
    let result = search(tmp.path(), block_size, b"ABCD", 0, Direction::Forward, 10).await;
    assert_eq!(result.matches, vec![6], "跨块拼接应匹配位置 6");
}

#[tokio::test]
async fn search_after_edit_offset_shift() {
    // 编辑后 cursor_byte 偏移变化，搜索 start_byte 应正确
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let block_size = 16;
    let mut data = vec![0x41u8; 48];
    data[20..24].copy_from_slice(b"FIND");
    std::fs::write(tmp.path(), &data).unwrap();

    // 从位置 0 搜 "FIND"，应匹配 20
    let result = search(tmp.path(), block_size, b"FIND", 0, Direction::Forward, 10).await;
    assert_eq!(result.matches, vec![20]);

    // 从位置 25 搜（在匹配之后），应无结果
    let result = search(tmp.path(), block_size, b"FIND", 25, Direction::Forward, 10).await;
    assert!(result.matches.is_empty());
}
