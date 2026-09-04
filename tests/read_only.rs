//! 只读模式测试：
//! - :ro / :rw 命令解析
//! - read_only 时编辑器拒绝缓冲修改（插入/删除/撤销/重做）

use std::sync::Arc;
use tokio::sync::mpsc;
use beditor::command::parse_command;
use beditor::command::CommandResult;
use beditor::config::EditorConfig;
use beditor::editor::Editor;
use beditor::OpenMode;

#[test]
fn command_ro_rw_parse() {
    assert!(matches!(parse_command("ro"), CommandResult::SetReadOnly(true)));
    assert!(matches!(parse_command("rw"), CommandResult::SetReadOnly(false)));
    assert!(matches!(parse_command("e"), CommandResult::SetReadOnly(false)));
}

async fn make_editor(path: &std::path::Path) -> Editor {
    let cfg = Arc::new(EditorConfig::default());
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

#[tokio::test]
async fn read_only_blocks_mutation() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"hello world").unwrap();
    let mut ed = make_editor(tmp.path()).await;
    ed.read_only = true;

    // 插入被拒绝
    ed.goto_byte(5);
    let r = ed.insert_bytes_at_cursor(b"XXX").await;
    assert!(r.is_err(), "只读模式下插入应失败");
    assert_eq!(ed.file_size, 11, "文件大小不应变化");
    assert!(ed.cache.dirty_ids().is_empty(), "不应产生脏块");

    // 删除被拒绝
    let r = ed.delete_bytes_at_cursor_forward(3).await;
    assert!(r.is_err(), "只读模式下删除应失败");

    // 撤销/重做被拒绝（即使栈里有内容也不应生效）
    ed.read_only = false;
    ed.insert_bytes_at_cursor(b"YY").await.unwrap();
    ed.read_only = true;
    assert!(ed.undo().await.is_err(), "只读模式下撤销应失败");
    assert_eq!(ed.file_size, 13, "撤销不应生效");

    // 切回可写后编辑恢复
    ed.read_only = false;
    ed.undo().await.unwrap();
    assert_eq!(ed.file_size, 11, "可写后撤销应生效");
}
