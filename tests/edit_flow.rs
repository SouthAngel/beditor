//! Task 13 Step 1: 端到端编辑流集成测试
//!
//! 在 4MB 随机文本（默认 256KB block_size → 16 块）上执行多次 insert/delete，
//! 保存到临时文件后与期望字节序列逐字节比对（强于哈希，等价于哈希一致性）。
//!
//! 注：plan 标称为 100MB；这里用 4MB 以保持 CI 友好。多块流程（split/merge/save）
//! 已被 16 块充分覆盖；如需压测可把 `DATA_LEN` 调到 100*1024*1024。

use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::config::EditorConfig;
use beditor::editor::Editor;
use beditor::OpenMode;

/// 简单确定性 PRNG（xorshift64）——保证测试可复现且无外部依赖。
fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn make_random_data(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let r = xorshift64(&mut state);
        // 取低 6 位限制到可打印 ASCII 范围 [0x20, 0x7E) 以便文本模式友好
        let b = (r & 0x3F) as u8;
        out.push(b.wrapping_add(0x20));
    }
    out
}

/// 计算 SHA-256 等价的“哈希一致性”此处用逐字节比较代替（更强）。
/// 该辅助函数仅用于在测试输出中给出可读摘要。
fn hex_short(data: &[u8]) -> String {
    // FNV-1a 64 位作为可读指纹，仅用于失败诊断
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

async fn make_editor(
    path: &std::path::Path,
    block_size: usize,
) -> Editor {
    let cfg = Arc::new(EditorConfig { block_size, ..Default::default() });
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _ev_rx) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        path.to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    Editor::open(
        path.to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap()
}

#[tokio::test]
async fn edit_flow_multi_insert_delete_save_consistency() {
    // 4MB 数据，block_size=256KB → 16 块
    const DATA_LEN: usize = 4 * 1024 * 1024;
    const BLOCK: usize = 256 * 1024;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = make_random_data(DATA_LEN, 0x1234_5678_dead_beef);
    std::fs::write(tmp.path(), &data).unwrap();

    let mut ed = make_editor(tmp.path(), BLOCK).await;
    assert_eq!(ed.file_size, DATA_LEN as u64);
    assert_eq!(ed.block_count, (DATA_LEN / BLOCK) as u64);

    // 期望缓冲：跟随编辑同步更新
    let mut expected = data.clone();

    // --- 编辑 1：在块 3 边界附近插入 ---
    let pos1: u64 = (3 * BLOCK + 100) as u64;
    ed.goto_byte(pos1);
    ed.insert_bytes_at_cursor(b"<<<INSERT_MARK_1>>>").await.unwrap();
    let mark1 = b"<<<INSERT_MARK_1>>>".to_vec();
    expected.splice(pos1 as usize..pos1 as usize, mark1.iter().cloned());
    assert_eq!(ed.file_size, expected.len() as u64);

    // --- 编辑 2：在块 7 中部删除块内字节（跨块删除是已知未实现项） ---
    let pos2: u64 = (7 * BLOCK + 100) as u64;
    ed.goto_byte(pos2);
    ed.delete_bytes_at_cursor_forward(120).await.unwrap();
    expected.drain(pos2 as usize..(pos2 as usize + 120));
    assert_eq!(ed.file_size, expected.len() as u64);

    // --- 编辑 3：在块 12 中部插入 ---
    let pos3: u64 = (12 * BLOCK + 2048) as u64;
    ed.goto_byte(pos3);
    ed.insert_bytes_at_cursor(b"<<<MARK_2>>>").await.unwrap();
    let mark2 = b"<<<MARK_2>>>".to_vec();
    expected.splice(pos3 as usize..pos3 as usize, mark2.iter().cloned());
    assert_eq!(ed.file_size, expected.len() as u64);

    // --- 编辑 4：在文件末尾追加 ---
    let pos4 = ed.file_size;
    ed.goto_byte(pos4);
    ed.insert_bytes_at_cursor(b"TAIL").await.unwrap();
    expected.extend_from_slice(b"TAIL");
    assert_eq!(ed.file_size, expected.len() as u64);

    // --- 保存并比对 ---
    let out = tmp.path().with_extension("edit_flow_out.bin");
    ed.save_as(out.clone()).await.unwrap();

    let got = std::fs::read(&out).unwrap();
    assert_eq!(got.len(), expected.len(),
        "长度不符 got={} expected={} (got_fingerprint={} expected_fingerprint={})",
        got.len(), expected.len(), hex_short(&got), hex_short(&expected));

    // 逐字节比对（强于哈希）
    assert_eq!(got, expected, "保存内容与期望不一致");

    // 找出首个不一致位置以便调试
    if got != expected {
        for i in 0..got.len().min(expected.len()) {
            if got[i] != expected[i] {
                panic!("首个不一致 @ byte {}: got={:02x} expected={:02x}",
                    i, got[i], expected[i]);
            }
        }
    }

    std::fs::remove_file(&out).ok();
}

#[tokio::test]
async fn edit_flow_save_idempotent_double_save() {
    // 同一编辑后连续两次保存到不同目标，结果应一致
    const DATA_LEN: usize = 512 * 1024;
    const BLOCK: usize = 64 * 1024;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = make_random_data(DATA_LEN, 0xfeed_face);
    std::fs::write(tmp.path(), &data).unwrap();

    let mut ed = make_editor(tmp.path(), BLOCK).await;
    let pos = 100_000u64;
    ed.goto_byte(pos);
    ed.insert_bytes_at_cursor(b"MARK").await.unwrap();

    let out1 = tmp.path().with_extension("idem1.bin");
    let out2 = tmp.path().with_extension("idem2.bin");
    ed.save_as(out1.clone()).await.unwrap();
    ed.save_as(out2.clone()).await.unwrap();

    let got1 = std::fs::read(&out1).unwrap();
    let got2 = std::fs::read(&out2).unwrap();
    assert_eq!(got1, got2, "两次保存结果应一致");

    // 与期望比对
    let mut expected = data.clone();
    expected.splice(pos as usize..pos as usize, b"MARK".iter().cloned());
    assert_eq!(got1, expected);

    std::fs::remove_file(&out1).ok();
    std::fs::remove_file(&out2).ok();
}

#[tokio::test]
async fn edit_flow_undo_redo_save_consistency() {
    // undo/redo 后保存应与对应状态一致
    const DATA_LEN: usize = 256 * 1024;
    const BLOCK: usize = 32 * 1024;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = make_random_data(DATA_LEN, 0xabc_def);
    std::fs::write(tmp.path(), &data).unwrap();

    let mut ed = make_editor(tmp.path(), BLOCK).await;
    let pos = 50_000u64;
    ed.goto_byte(pos);
    ed.insert_bytes_at_cursor(b"X").await.unwrap();
    ed.insert_bytes_at_cursor(b"Y").await.unwrap();
    // 现在 expected = data[..pos] + "XY" + data[pos..]
    let mut expected_xy = data.clone();
    expected_xy.splice(pos as usize..pos as usize, b"XY".iter().cloned());

    // undo 两次 → 应回到原始数据
    ed.undo().await.unwrap();
    ed.undo().await.unwrap();
    assert_eq!(ed.file_size, DATA_LEN as u64);

    let out_orig = tmp.path().with_extension("undo_orig.bin");
    ed.save_as(out_orig.clone()).await.unwrap();
    let got = std::fs::read(&out_orig).unwrap();
    assert_eq!(got, data, "undo 后保存应等于原始数据");
    std::fs::remove_file(&out_orig).ok();

    // redo 两次 → 应等于 expected_xy
    ed.redo().await.unwrap();
    ed.redo().await.unwrap();
    let out_redo = tmp.path().with_extension("redo_xy.bin");
    ed.save_as(out_redo.clone()).await.unwrap();
    let got = std::fs::read(&out_redo).unwrap();
    assert_eq!(got, expected_xy, "redo 后保存应等于编辑后数据");
    std::fs::remove_file(&out_redo).ok();
}

/// undo 应把光标恢复到操作前位置（即使插入后光标已移到别处）
#[tokio::test]
async fn undo_restores_cursor_to_insert_point() {
    const BLOCK: usize = 32 * 1024;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = vec![0x41u8; 1000];
    std::fs::write(tmp.path(), &data).unwrap();
    let mut ed = make_editor(tmp.path(), BLOCK).await;

    let pos = 100u64;
    ed.goto_byte(pos);
    ed.insert_bytes_at_cursor(b"HELLO").await.unwrap();
    assert_eq!(ed.cursor_byte, pos + 5);

    // 把光标移到别处再 undo → 应回到插入点
    ed.goto_byte(500);
    ed.undo().await.unwrap();
    assert_eq!(ed.cursor_byte, pos, "undo 插入后光标应回到插入点");

    // redo → 光标应到插入内容末尾
    ed.redo().await.unwrap();
    assert_eq!(ed.cursor_byte, pos + 5, "redo 插入后光标应到插入内容末尾");
}

/// undo 删除后光标应恢复到删除起点（而非从当前光标倒退）
#[tokio::test]
async fn undo_restores_cursor_to_delete_start() {
    const BLOCK: usize = 32 * 1024;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = vec![0x42u8; 1000];
    std::fs::write(tmp.path(), &data).unwrap();
    let mut ed = make_editor(tmp.path(), BLOCK).await;

    let pos = 100u64;
    ed.goto_byte(pos);
    ed.delete_bytes_at_cursor_forward(5).await.unwrap();

    // 把光标移到别处再 undo → 应回到删除起点
    ed.goto_byte(900);
    ed.undo().await.unwrap();
    assert_eq!(ed.cursor_byte, pos, "undo 删除后光标应回到删除起点");

    // redo → 光标保持在删除起点
    ed.redo().await.unwrap();
    assert_eq!(ed.cursor_byte, pos, "redo 删除后光标保持在删除起点");
}

/// 搜索应命中未保存的内存编辑（旧版纯磁盘搜索搜不到新插入的内容），
/// 且编辑引起的偏移变化应被正确计算。
#[tokio::test]
async fn search_finds_unsaved_edits() {
    use beditor::message::Direction;
    const BLOCK: usize = 32 * 1024;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = b"AAAAfind original BBBB".to_vec();
    std::fs::write(tmp.path(), &data).unwrap();
    let mut ed = make_editor(tmp.path(), BLOCK).await;

    // 在位置 4 前插入 "find-new-"（仅存在于内存，尚未保存）
    ed.goto_byte(4);
    ed.insert_bytes_at_cursor(b"find-new-").await.unwrap();

    // 光标在插入内容之后，先回文件头再正向搜索
    ed.goto_byte(0);
    let r = ed.search_literal(b"find-new", Direction::Forward, 10).await.unwrap();
    assert_eq!(r.matches, vec![4], "应命中内存中新插入的内容");
    // 反向搜索：光标移到文件末尾
    ed.goto_byte(ed.file_size);
    let r2 = ed.search_literal(b"find-new", Direction::Backward, 10).await.unwrap();
    assert_eq!(r2.matches, vec![4]);

    // 原内容因插入偏移 9 字节：original 从 9 → 18
    ed.goto_byte(0);
    let r3 = ed.search_literal(b"original", Direction::Forward, 10).await.unwrap();
    assert_eq!(r3.matches, vec![18], "编辑偏移后的原内容位置应正确");
}

/// 分裂后搜索：内存感知搜索在非均匀块布局下仍能找到跨块内容。
#[tokio::test]
async fn search_after_split_finds_content() {
    use beditor::message::Direction;
    const BLOCK: usize = 32;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let data = vec![0x41u8; 32];
    std::fs::write(tmp.path(), &data).unwrap();
    let mut ed = make_editor(tmp.path(), BLOCK).await;

    // 插入触发分裂（32+48=80 > 1.5*32）
    ed.goto_byte(0);
    ed.insert_bytes_at_cursor(&[0x42u8; 48]).await.unwrap();
    assert!(ed.block_count >= 2);
    // 光标在 48，先回文件头再正向搜索
    ed.goto_byte(0);

    // 搜索分裂产生的两块都有的内容（0x42）
    let r = ed.search_literal(&[0x42u8; 4], Direction::Forward, 1).await.unwrap();
    assert_eq!(r.matches, vec![0], "前 48 字节都是 0x42");
    // 跨块匹配：块0 = 0x42×40，块1 = 0x42×8 + 0x41×32。
    // 模式 [0x42;10]+0x41 从块 0 尾部(38) 跨入块 1(40..48) 再到 0x41(48) → 匹配位置 38
    let mut pattern = vec![0x42u8; 10];
    pattern.push(0x41);
    let cross = ed.search_literal(&pattern, Direction::Forward, 10).await.unwrap();
    assert_eq!(cross.matches, vec![38], "跨块边界应匹配到位置 38");
}
