//! WAL（Write-Ahead Log）增量持久化测试：
//! - 追加/读取/重置、同块后者覆盖、崩溃回放、半截条目容错
//! - 脏块淘汰时刷入 WAL（内存压力不丢编辑）
//! - :w 增量保存写入 WAL；save_as 合并 WAL 内容；重开回放

use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::block::Block;
use beditor::cache::BlockCache;
use beditor::config::EditorConfig;
use beditor::editor::Editor;
use beditor::wal::Wal;
use beditor::OpenMode;

fn tmp_wal_path(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    tmp.path().join("test.beditor-wal")
}

#[test]
fn wal_append_get_reset() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wal = Wal::open(tmp_wal_path(&tmp), true).unwrap().unwrap();
    assert!(!wal.has_entries());
    wal.append_many(&[(0, b"hello".to_vec()), (1, b"world".to_vec())]).unwrap();
    assert!(wal.has_entries());
    assert_eq!(wal.len(), 2);
    assert_eq!(wal.get(0).unwrap(), Some(b"hello".to_vec()));
    assert_eq!(wal.get(1).unwrap(), Some(b"world".to_vec()));
    assert_eq!(wal.get(9).unwrap(), None);
    wal.reset().unwrap();
    assert!(!wal.has_entries());
    assert_eq!(wal.get(0).unwrap(), None);
}

#[test]
fn wal_open_nonexistent_create_false() {
    // create=false 且文件不存在 → Ok(None)，不生成临时文件
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp_wal_path(&tmp);
    assert!(Wal::open(path.clone(), false).unwrap().is_none());
    assert!(!path.exists(), "create=false 不应创建文件");
}

#[test]
fn wal_same_block_latest_wins() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wal = Wal::open(tmp_wal_path(&tmp), true).unwrap().unwrap();
    wal.append_one(0, b"v1").unwrap();
    wal.append_one(0, b"v2-longer").unwrap();
    assert_eq!(wal.get(0).unwrap(), Some(b"v2-longer".to_vec()));
}

#[test]
fn wal_replay_after_reopen() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let wal = Wal::open(tmp_wal_path(&tmp), true).unwrap().unwrap();
        wal.append_one(3, b"persisted").unwrap();
    } // drop，模拟进程结束
    let wal2 = Wal::open(tmp_wal_path(&tmp), false).unwrap().unwrap();
    assert!(wal2.has_entries());
    assert_eq!(wal2.get(3).unwrap(), Some(b"persisted".to_vec()));
}

#[test]
fn wal_partial_tail_ignored() {
    // 模拟崩溃：写一条完整条目 + 一条半截条目，半截应被忽略
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp_wal_path(&tmp);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        f.write_all(&4u32.to_le_bytes()).unwrap();
        f.write_all(b"AAAA").unwrap();
        f.write_all(&2u32.to_le_bytes()).unwrap();
        f.write_all(&8u32.to_le_bytes()).unwrap();
        f.write_all(b"XYZ").unwrap(); // 只有 3/8 字节
    }
    let wal = Wal::open(path, true).unwrap().unwrap();
    assert_eq!(wal.get(1).unwrap(), Some(b"AAAA".to_vec()));
    assert_eq!(wal.get(2).unwrap(), None, "半截条目应被忽略");
}

/// 脏块被 LRU 淘汰前必须写入 WAL，避免内存压力丢失未保存编辑。
#[test]
fn eviction_flushes_dirty_block_to_wal() {
    let cfg = Arc::new(EditorConfig::default());
    let tmp = tempfile::TempDir::new().unwrap();
    let wal = Wal::open(tmp_wal_path(&tmp), true).unwrap().unwrap();
    let cache = Arc::new(BlockCache::new_for_test(cfg, 1));
    cache.attach_wal(wal.clone());

    // 块 0：clean
    cache.insert_loaded(Block::new_clean(0, b"original0".to_vec()));
    // 编辑块 0 → dirty
    let _g = cache.pin_for_edit(0).unwrap();
    cache.with_pin_mut(0, |gb| gb.insert(0, b"EDITED")).unwrap();
    drop(_g);

    // 插入块 1 → 容量 1 → 块 0 被淘汰（脏 → 写入 WAL）
    cache.insert_loaded(Block::new_clean(1, b"block1".to_vec()));
    assert!(cache.try_get(0).is_none(), "块 0 应已被淘汰");
    assert!(wal.has_entries(), "脏块被淘汰前应写入 WAL");
    let w = wal.get(0).unwrap().unwrap();
    assert!(w.windows(6).any(|x| x == b"EDITED"), "WAL 应含编辑后内容, got {:?}", w);
}

async fn make_editor(path: &std::path::Path, block_size: usize) -> Editor {
    let cfg = Arc::new(EditorConfig { block_size, ..Default::default() });
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        path.to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    Editor::open(
        path.to_path_buf(), cfg, OpenMode::Text, encoding_rs::UTF_8,
        io_tx, cache,
    ).await.unwrap()
}

/// :w 增量保存：脏块写入 WAL 并标记 clean，不改动基础文件。
#[tokio::test]
async fn save_incremental_persists_to_wal() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"AAAAAAAAAA").unwrap();
    let mut ed = make_editor(tmp.path(), 32).await;
    ed.goto_byte(2);
    ed.insert_bytes_at_cursor(b"NEW").await.unwrap();
    assert!(!ed.cache.dirty_ids().is_empty());

    ed.save_incremental().await.unwrap();
    assert!(ed.cache.dirty_ids().is_empty(), ":w 后脏块应清空");
    assert!(ed.wal_pending(), ":w 后应有未合并 WAL 增量");

    let wal = ed.cache.wal().unwrap();
    let w = wal.get(0).unwrap().unwrap();
    assert!(w.windows(3).any(|x| x == b"NEW"), "WAL 应含编辑后内容, got {:?}", w);
}

/// save_as 合并时，WAL 中已有的块内容应被采用（模拟脏块已淘汰但已持久化）。
#[tokio::test]
async fn save_as_merges_wal_content() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"AAAAAAAAAA").unwrap();
    let mut ed = make_editor(tmp.path(), 32).await;
    // 惰性 WAL：需先显式启用，才能拿到 WAL 引用写块内容
    ed.ensure_wal().await.unwrap();
    let wal = ed.cache.wal().unwrap();
    let edited0 = b"XNEWAAAAAAAAA".to_vec(); // 块 0 编辑后（长度变化）
    wal.append_one(0, &edited0).unwrap();

    let out = tmp.path().with_file_name("merged.bin");
    ed.save_as(out.clone()).await.unwrap();
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, edited0, "save_as 应采用 WAL 中的块内容");
    std::fs::remove_file(&out).ok();
}

/// 只读/未编辑模式：启动不生成 WAL 临时文件；首次编辑才惰性创建。
#[tokio::test]
async fn lazy_wal_no_file_until_first_edit() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"AAAAAAAAAA").unwrap();
    let mut ed = make_editor(tmp.path(), 32).await;
    let wal_path = tmp.path().with_extension("beditor-wal");

    assert!(ed.cache.wal().is_none(), "未编辑前不应挂接 WAL");
    assert!(!wal_path.exists(), "未编辑前不应生成 WAL 临时文件");

    ed.goto_byte(2);
    ed.insert_bytes_at_cursor(b"NEW").await.unwrap();
    assert!(ed.cache.wal().is_some(), "首次编辑后应挂接 WAL");
    assert!(wal_path.exists(), "首次编辑后应生成 WAL 临时文件");
}

/// 完整合并写入基础文件（save_as 折叠到自身）后，WAL 临时文件应被删除。
/// 折叠到自身仅出现在退出路径，此处额外验证删除后 ensure_wal 可重新启用。
#[tokio::test]
async fn fold_save_removes_wal_file() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"AAAAAAAAAA").unwrap();
    let mut ed = make_editor(tmp.path(), 32).await;
    let wal_path = tmp.path().with_extension("beditor-wal");

    ed.goto_byte(2);
    ed.insert_bytes_at_cursor(b"NEW").await.unwrap();
    assert!(wal_path.exists(), "编辑后 WAL 已创建");

    ed.save_as(tmp.path().to_path_buf()).await.unwrap(); // 折叠到自身
    assert!(!wal_path.exists(), "合并保存后应删除 WAL 临时文件");
    assert!(!ed.wal_pending(), "折叠后不应有未合并 WAL 增量");

    // 删除后如需继续写会话：ensure_wal 应重新创建临时文件
    ed.ensure_wal().await.unwrap();
    assert!(wal_path.exists(), "ensure_wal 应重新创建 WAL 临时文件");
    assert!(ed.cache.wal().is_some());
}

/// 崩溃后重开：io_worker 回放 WAL，未保存编辑自动恢复。
#[tokio::test]
async fn reopen_replays_wal() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"AAAAAAAAAA").unwrap();
    // 第一次会话：编辑并增量保存到 WAL
    {
        let mut ed = make_editor(tmp.path(), 32).await;
        ed.goto_byte(2);
        ed.insert_bytes_at_cursor(b"NEW").await.unwrap();
        ed.save_incremental().await.unwrap();
    } // ed 与 workers 一起结束（WAL 已 fsync 落盘）

    // 重新打开：应检测到并回放 WAL
    let ed2 = make_editor(tmp.path(), 32).await;
    assert!(ed2.wal_pending(), "重新打开应检测到 WAL");
    let snap = ed2.ensure_block_loaded(0).await.unwrap();
    assert!(
        snap.contiguous.windows(3).any(|x| x == b"NEW"),
        "应从 WAL 回放编辑内容, got {:?}",
        snap.contiguous
    );
}
