use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::config::EditorConfig;
use beditor::editor::Editor;
use beditor::OpenMode;

#[tokio::test]
async fn undo_insert() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"Hello World").unwrap();
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
    let orig_size = ed.file_size;
    ed.goto_byte(5);
    ed.insert_bytes_at_cursor(b" XXX ").await.unwrap();
    assert_eq!(ed.file_size, orig_size + 5);
    ed.undo().await.unwrap();
    assert_eq!(ed.file_size, orig_size);
    let out = tmp.path().with_file_name("undo.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, b"Hello World");
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn redo_after_undo() {
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
    ed.goto_byte(3);
    ed.insert_bytes_at_cursor(b"XYZ").await.unwrap();
    assert_eq!(ed.file_size, 9);
    ed.undo().await.unwrap();
    assert_eq!(ed.file_size, 6);
    ed.redo().await.unwrap();
    assert_eq!(ed.file_size, 9);
    let out = tmp.path().with_file_name("redo.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, b"ABCXYZDEF");
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn insert_trigger_split() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let init = vec![0x41u8; 64];
    std::fs::write(tmp.path(), &init).unwrap();
    let cfg = Arc::new(EditorConfig { block_size: 32, ..Default::default() });
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    ed.goto_byte(30);
    ed.insert_bytes_at_cursor(&[0x42u8; 50]).await.unwrap();
    assert!(ed.block_count >= 2, "分裂后块数应增加, got {}", ed.block_count);
    let out = tmp.path().with_file_name("split.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    let mut exp = vec![0x41u8; 64];
    let right = exp.split_off(30);
    exp.extend(vec![0x42u8; 50]);
    exp.extend(right);
    assert_eq!(got.len(), 114);
    assert_eq!(got, exp);
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn stream_save_multi_block() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 64, ..Default::default() };
    let data = vec![0xAAu8; 1000];
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    ed.goto_byte(100);
    ed.insert_bytes_at_cursor(b"TEST").await.unwrap();
    assert_eq!(ed.file_size, 1004);
    let out = tmp.path().with_file_name("stream.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got.len(), 1004);
    let mut exp = Vec::with_capacity(1004);
    exp.extend_from_slice(&data[..100]);
    exp.extend_from_slice(b"TEST");
    exp.extend_from_slice(&data[100..]);
    assert_eq!(got, exp);
    std::fs::remove_file(&out).ok();
}

// ---- Task 13 Step 2 补充：边界分裂合并 ----

#[tokio::test]
async fn insert_at_exact_block_boundary() {
    // 在块边界（block 1 起点）插入
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 32, ..Default::default() };
    let data = vec![0x41u8; 96]; // 3 块
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    // 在逻辑位置 32（块 1 起点）插入
    ed.goto_byte(32);
    ed.insert_bytes_at_cursor(b"XYZ").await.unwrap();
    assert_eq!(ed.file_size, 99);
    let out = tmp.path().with_file_name("boundary.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    let mut exp = Vec::with_capacity(99);
    exp.extend_from_slice(&data[..32]);
    exp.extend_from_slice(b"XYZ");
    exp.extend_from_slice(&data[32..]);
    assert_eq!(got, exp);
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn delete_at_block_boundary() {
    // 在块边界附近删除（块内删除，不跨块）
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 32, ..Default::default() };
    let data: Vec<u8> = (0..96).map(|i| (i % 26) as u8 + b'a').collect();
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    // 在块 1 的 inner 10 处删除 12 字节（不跨块）
    ed.goto_byte(42); // 32 + 10
    ed.delete_bytes_at_cursor_forward(12).await.unwrap();
    assert_eq!(ed.file_size, 84);
    let out = tmp.path().with_file_name("del_boundary.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    let mut exp = data.clone();
    exp.drain(42..54);
    assert_eq!(got, exp);
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn split_then_save() {
    // 通过 insert 触发分裂，验证保存后内容正确
    // 注：undo-after-split 需要跨块删除（未实现），此处只验证分裂+保存
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 32, ..Default::default() };
    let data = vec![0x41u8; 32]; // 1 块
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    assert_eq!(ed.block_count, 1);
    // 插入 48 字节 → 32+48=80 > 1.5*32=48 → 触发分裂
    ed.goto_byte(0);
    ed.insert_bytes_at_cursor(&[0x42u8; 48]).await.unwrap();
    assert_eq!(ed.block_count, 2, "插入后应分裂为 2 块");
    assert_eq!(ed.file_size, 80);
    let out = tmp.path().with_file_name("split_save.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    let mut exp = Vec::with_capacity(80);
    exp.extend_from_slice(&[0x42u8; 48]);
    exp.extend_from_slice(&data);
    assert_eq!(got.len(), 80);
    assert_eq!(got, exp);
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn multiple_inserts_trigger_multiple_splits() {
    // 分多次插入，每次触发分裂（仅最后块可分裂）
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 32, ..Default::default() };
    let data = vec![0x41u8; 32]; // 1 块
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    // 分 3 次在末尾插入，每次 40 字节（32+40=72 > 48 阈值）
    for _ in 0..3 {
        ed.goto_byte(ed.file_size);
        ed.insert_bytes_at_cursor(&[0x43u8; 40]).await.unwrap();
    }
    assert!(ed.block_count >= 3, "3 次插入应产生 >= 3 块, got {}", ed.block_count);
    assert_eq!(ed.file_size, 152); // 32 + 40*3
    let out = tmp.path().with_file_name("multi_split.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    let mut exp = Vec::with_capacity(152);
    exp.extend_from_slice(&data);
    exp.extend_from_slice(&[0x43u8; 120]);
    assert_eq!(got, exp);
    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn edit_multiple_blocks_then_undo_all() {
    // 在多个块编辑后全部 undo，验证保存等于原始
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 16, ..Default::default() };
    let data: Vec<u8> = (0..128).map(|i| (i % 26) as u8 + b'a').collect();
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    let orig_size = ed.file_size;
    // 在块 2, 5, 7 各插入
    for &pos in &[32u64, 80, 112] {
        ed.goto_byte(pos);
        ed.insert_bytes_at_cursor(b"QQ").await.unwrap();
    }
    assert_eq!(ed.file_size, orig_size + 6);
    // 全部 undo
    for _ in 0..3 {
        ed.undo().await.unwrap();
    }
    assert_eq!(ed.file_size, orig_size);
    let out = tmp.path().with_file_name("undo_all.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, data, "全部 undo 后保存应等于原始");
    std::fs::remove_file(&out).ok();
}

/// undo-after-split：插入触发分裂后 undo，应先合并回分裂（块数还原），
/// 再撤销插入，内容完全还原。
#[tokio::test]
async fn undo_after_split() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 32, ..Default::default() };
    let data = vec![0x41u8; 32];
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    assert_eq!(ed.block_count, 1);
    // 插入 48 字节 → 32+48=80 > 1.5*32 → 触发分裂
    ed.goto_byte(0);
    ed.insert_bytes_at_cursor(&[0x42u8; 48]).await.unwrap();
    assert_eq!(ed.block_count, 2, "插入后应分裂为 2 块");
    assert_eq!(ed.file_size, 80);

    // undo：先合并分裂，再撤销插入
    ed.undo().await.unwrap();
    assert_eq!(ed.block_count, 1, "undo 后应合并回 1 块");
    assert_eq!(ed.file_size, 32, "undo 后文件应还原");

    let out = tmp.path().with_file_name("undo_split.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, data, "undo 后保存应等于原始");
    std::fs::remove_file(&out).ok();
}

/// undo-after-split 后再 redo：先重放插入，再重新分裂，内容与保存一致。
#[tokio::test]
async fn redo_after_split_undo() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let cfg = EditorConfig { block_size: 32, ..Default::default() };
    let data = vec![0x41u8; 32];
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(cfg);
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap();
    ed.goto_byte(0);
    ed.insert_bytes_at_cursor(&[0x42u8; 48]).await.unwrap();
    assert_eq!(ed.block_count, 2);

    // 验证分裂后 delta_table 一致：offset 60 应落在块 1
    let (bid, inner) = ed.delta_table.locate_offset(60);
    assert_eq!((bid, inner), (1, 20), "分裂后 offset 60 应位于块 1 的 inner 20");

    ed.undo().await.unwrap();
    assert_eq!(ed.block_count, 1);
    ed.redo().await.unwrap();
    assert_eq!(ed.block_count, 2, "redo 后应恢复 2 块");
    assert_eq!(ed.file_size, 80);

    let out = tmp.path().with_file_name("redo_split.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    let mut exp = Vec::with_capacity(80);
    exp.extend_from_slice(&[0x42u8; 48]);
    exp.extend_from_slice(&data);
    assert_eq!(got, exp, "redo 后保存应等于分裂后内容");
    std::fs::remove_file(&out).ok();
}
