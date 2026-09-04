use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::config::EditorConfig;
use beditor::editor::Editor;
use beditor::OpenMode;

#[tokio::test]
async fn open_small_cursor_move_insert() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let init = b"0123456789".repeat(10); // 100B
    std::fs::write(tmp.path(), &init).unwrap();
    let cfg = Arc::new(EditorConfig { block_size: 32, ..Default::default() }); // 强制多块
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _ev_rx) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    assert_eq!(ed.file_size, 100);
    assert_eq!(ed.cursor_byte, 0);
    ed.goto_byte(50);
    assert_eq!(ed.cursor_byte, 50);
    // 插入 2 字节 "XY" 在位置 50
    ed.insert_bytes_at_cursor(b"XY").await.unwrap();
    assert_eq!(ed.file_size, 102);
    assert_eq!(ed.cursor_byte, 52);
    // 删除 cursor 位置向前 1 字节（删除位置 52 的字节）
    ed.delete_bytes_at_cursor_forward(1).await.unwrap();
    assert_eq!(ed.file_size, 101);
    // 保存验证
    let out = tmp.path().with_file_name("ed_test.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    // 期望: init[0..50] + "XY" + init[51..]  (删除了 init[50])
    let mut exp = Vec::with_capacity(101);
    exp.extend_from_slice(&init[..50]);
    exp.extend_from_slice(b"XY");
    exp.extend_from_slice(&init[51..]);
    assert_eq!(got, exp);
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn open_empty_file() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"").unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    assert_eq!(ed.file_size, 0);
    // 空文件插入
    ed.insert_bytes_at_cursor(b"Hello").await.unwrap();
    assert_eq!(ed.file_size, 5);
    assert_eq!(ed.cursor_byte, 5);
    let out = tmp.path().with_file_name("ed_empty.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, b"Hello");
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn insert_at_end() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"ABCDEF").unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    ed.goto_byte(6); // 末尾
    ed.insert_bytes_at_cursor(b"GHI").await.unwrap();
    assert_eq!(ed.file_size, 9);
    let out = tmp.path().with_file_name("ed_end.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, b"ABCDEFGHI");
    std::fs::remove_file(&out).ok();
}
