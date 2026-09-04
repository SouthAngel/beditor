use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::config::EditorConfig;
use beditor::message::*;
use beditor::io_worker::spawn_io_workers;

#[tokio::test]
async fn load_sequential_blocks() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data: Vec<u8> = (0u64..4)
        .flat_map(|i| std::iter::repeat(i as u8).take(256 * 1024))
        .collect();
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (event_tx, _event_rx) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg.clone(), io_rx, event_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::LoadBlock { block_id: 3, reply: tx }).await.unwrap();
    let block_bytes = rx.await.unwrap().unwrap();
    assert_eq!(block_bytes.len(), 256 * 1024);
    assert!(block_bytes.iter().all(|b| *b == 3), "所有字节应该为 3");
}

#[tokio::test]
async fn partial_last_block() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let size = 256 * 1024 + 42;
    std::fs::write(tmp.path(), vec![0xABu8; size]).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (event_tx, _) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg, io_rx, event_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::LoadBlock { block_id: 1, reply: tx }).await.unwrap();
    let bytes = rx.await.unwrap().unwrap();
    assert_eq!(bytes.len(), 42);
}

#[tokio::test]
async fn read_file_header() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pattern = b"HEADER-ABC123";
    let mut body = pattern.to_vec();
    body.extend(std::iter::repeat(0x00).take(1_000_000));
    std::fs::write(tmp.path(), &body).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel::<IoRequest>(16);
    let (event_tx, _) = mpsc::channel::<IoEvent>(16);
    let _cache = spawn_io_workers(tmp.path().to_path_buf(), cfg, io_rx, event_tx).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    io_tx.send(IoRequest::ReadFileHeader { max_bytes: 64, reply: tx }).await.unwrap();
    let header = rx.await.unwrap();
    assert!(header.len() <= 64);
    assert_eq!(&header[..13], pattern);
}
