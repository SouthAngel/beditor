use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::config::EditorConfig;
use beditor::message::*;
use beditor::io_worker::spawn_io_workers;

#[tokio::test]
async fn search_forward_basic() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    // "abc" 在位置 0, 10, 20
    let data = b"abcXXXXXXXabcXXXXXXXabc".to_vec();
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (ev_tx, _) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg, io_rx, ev_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::SearchLiteral {
        query: b"abc".to_vec(),
        start_byte: 0,
        start_block: 0,
        start_block_offset: 0,
        logical_block_count: 1,
        direction: Direction::Forward,
        limit: 10,
        reply: tx,
    }).await.unwrap();
    let result = rx.await.unwrap();
    assert_eq!(result.matches, vec![0, 10, 20]);
    assert!(!result.has_more_behind);
}

#[tokio::test]
async fn search_forward_with_limit() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = b"abcXXXabcXXXabcXXXabc".to_vec();
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (ev_tx, _) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg, io_rx, ev_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::SearchLiteral {
        query: b"abc".to_vec(),
        start_byte: 0,
        start_block: 0,
        start_block_offset: 0,
        logical_block_count: 1,
        direction: Direction::Forward,
        limit: 2,
        reply: tx,
    }).await.unwrap();
    let result = rx.await.unwrap();
    assert_eq!(result.matches, vec![0, 6]);
    assert!(result.has_more_behind);
}

#[tokio::test]
async fn search_forward_from_offset() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = b"abcXXXabcXXXabc".to_vec();
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (ev_tx, _) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg, io_rx, ev_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::SearchLiteral {
        query: b"abc".to_vec(),
        start_byte: 5,
        start_block: 0,
        start_block_offset: 0,
        logical_block_count: 1,
        direction: Direction::Forward,
        limit: 10,
        reply: tx,
    }).await.unwrap();
    let result = rx.await.unwrap();
    // 从位置 5 开始搜，应该匹配位置 6 和 12
    assert_eq!(result.matches, vec![6, 12]);
}

#[tokio::test]
async fn search_backward_basic() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = b"abcXXXabcXXXabc".to_vec();
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (ev_tx, _) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg, io_rx, ev_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::SearchLiteral {
        query: b"abc".to_vec(),
        start_byte: 13,
        start_block: 0,
        start_block_offset: 0,
        logical_block_count: 1,
        direction: Direction::Backward,
        limit: 10,
        reply: tx,
    }).await.unwrap();
    let result = rx.await.unwrap();
    // 从位置 13 向前搜，应匹配 12, 6, 0（倒序）
    assert_eq!(result.matches, vec![12, 6, 0]);
}

#[tokio::test]
async fn search_not_found() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"hello world no match here").unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (ev_tx, _) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg, io_rx, ev_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::SearchLiteral {
        query: b"xyz".to_vec(),
        start_byte: 0,
        start_block: 0,
        start_block_offset: 0,
        logical_block_count: 1,
        direction: Direction::Forward,
        limit: 10,
        reply: tx,
    }).await.unwrap();
    let result = rx.await.unwrap();
    assert!(result.matches.is_empty());
}
