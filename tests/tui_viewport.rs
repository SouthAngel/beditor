use beditor::tui::viewport::ViewPort;
use beditor::tui::render_text::{render_text_line, expand_tabs, truncate_line};

#[test]
fn viewport_new() {
    let vp = ViewPort::new(24, 80);
    assert_eq!(vp.visible_rows, 24);
    assert_eq!(vp.visible_cols, 80);
    assert_eq!(vp.top_byte, 0);
}

#[test]
fn viewport_scroll() {
    let vp = ViewPort::new(24, 80);
    // scroll_lines 需要 editor，这里只验证 ViewPort 构造
    assert_eq!(vp.top_byte, 0);
}

#[test]
fn render_text_basic() {
    let cfg = beditor::config::EditorConfig::default();
    let line = render_text_line(b"Hello World", encoding_rs::UTF_8, &cfg, None, &[]);
    // Line 应有至少 1 个 Span
    assert!(!line.spans.is_empty());
}

#[test]
fn render_text_with_tab() {
    let result = expand_tabs("A\tB", 4);
    assert_eq!(result, "A   B"); // tab 在 col 1 → 3 spaces
}

#[test]
fn render_text_tab_at_col0() {
    let result = expand_tabs("\tX", 4);
    assert_eq!(result, "    X"); // tab at col 0 → 4 spaces
}

#[test]
fn truncate_long_line() {
    let s = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let t = truncate_line(s, 10);
    assert_eq!(t, "ABCDEFGHIJ");
}

#[test]
fn truncate_short_line() {
    let s = "ABC";
    let t = truncate_line(s, 10);
    assert_eq!(t, "ABC");
}

#[tokio::test]
async fn cursor_screen_pos_text_and_hex() {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use beditor::config::EditorConfig;
    use beditor::editor::Editor;
    use beditor::OpenMode;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    // 前 15 字节为三行变长文本，之后补足 100 字节供 hex 模式跨行
    let mut content = b"aaaa\nbbbb\ncccc\n".to_vec();
    content.resize(100, 0x41);
    std::fs::write(tmp.path(), &content).unwrap(); // 行0:[0..5) 行1:[5..10) 行2:[10..15)
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();

    // 文本模式：按真实换行映射行/列
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg.clone(), OpenMode::Text,
        encoding_rs::UTF_8, io_tx.clone(), cache.clone(),
    ).await.unwrap();
    let mut vp = ViewPort::new(24, 80);
    ed.goto_byte(0);
    assert_eq!(vp.cursor_screen_pos(&ed, 16).await, Some((0, 0)));
    ed.goto_byte(5); // 行 1 起点
    assert_eq!(vp.cursor_screen_pos(&ed, 16).await, Some((1, 0)));
    ed.goto_byte(7); // 行 1 列 2
    assert_eq!(vp.cursor_screen_pos(&ed, 16).await, Some((1, 2)));
    ed.goto_byte(14); // 行 2 末尾换行符
    assert_eq!(vp.cursor_screen_pos(&ed, 16).await, Some((2, 4)));

    // Hex 模式：col = 10 + inner*3
    let mut edh = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary,
        encoding_rs::UTF_8, io_tx, cache,
    ).await.unwrap();
    let mut vph = ViewPort::new(24, 80);
    edh.goto_byte(0);
    assert_eq!(vph.cursor_screen_pos(&edh, 16).await, Some((0, 10)));
    edh.goto_byte(1);
    assert_eq!(vph.cursor_screen_pos(&edh, 16).await, Some((0, 13)));
    edh.goto_byte(16);
    assert_eq!(vph.cursor_screen_pos(&edh, 16).await, Some((1, 10)));
}

#[tokio::test]
async fn move_cursor_lines_follows_viewport() {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use beditor::config::EditorConfig;
    use beditor::editor::Editor;
    use beditor::OpenMode;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    // 100 行 "lineXXX\n"，每行 8 字节，第 k 行起始 = k*8
    let mut data = Vec::new();
    for i in 0..100 {
        data.extend_from_slice(format!("line{i:03}\n").as_bytes());
    }
    std::fs::write(tmp.path(), &data).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();

    // 文本模式：j/k 按真实逻辑行移动，列保持
    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg.clone(), OpenMode::Text,
        encoding_rs::UTF_8, io_tx.clone(), cache.clone(),
    ).await.unwrap();
    let mut vp = ViewPort::new(24, 80);
    ed.goto_byte(40 + 3); // 行 5 列 3
    ed.move_cursor_lines(1, vp.bytes_per_line(&ed), &mut vp.line_index).await;
    assert_eq!(ed.cursor_byte, 48 + 3, "下移一行应到第 6 行同列");
    assert_eq!(vp.cursor_screen_pos(&ed, 16).await, Some((6, 3)));
    ed.move_cursor_lines(-1, vp.bytes_per_line(&ed), &mut vp.line_index).await;
    assert_eq!(ed.cursor_byte, 40 + 3);
    // 连续下移超出视口 → ensure_cursor_visible 应滚动保持可见
    for _ in 0..80 {
        ed.move_cursor_lines(1, vp.bytes_per_line(&ed), &mut vp.line_index).await;
        vp.ensure_cursor_visible(&ed).await;
        assert!(vp.cursor_screen_pos(&ed, 16).await.is_some(), "光标应始终在视口内");
    }

    // Hex 模式：16B/行
    let mut edh = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary,
        encoding_rs::UTF_8, io_tx, cache,
    ).await.unwrap();
    let mut vph = ViewPort::new(24, 80);
    edh.goto_byte(20);
    edh.move_cursor_lines(1, vph.bytes_per_line(&edh), &mut vph.line_index).await;
    assert_eq!(edh.cursor_byte, 36); // 20 + 16
    assert_eq!(vph.cursor_screen_pos(&edh, 16).await, Some((2, 22))); // 列保持 4 → col=10+4*3
    // 连续下移超出视口 → 滚动保持可见
    for _ in 0..40 {
        edh.move_cursor_lines(1, vph.bytes_per_line(&edh), &mut vph.line_index).await;
        vph.ensure_cursor_visible(&edh).await;
        assert!(vph.cursor_screen_pos(&edh, 16).await.is_some(), "hex 光标应始终在视口内");
    }
}

/// 窄终端（<40 列）：hex 行宽降级为 8B/行，光标移动/可见性/滚动必须与渲染一致
#[tokio::test]
async fn narrow_terminal_hex_bytes_per_line() {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use beditor::config::EditorConfig;
    use beditor::editor::Editor;
    use beditor::OpenMode;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), vec![0x41u8; 10_000]).unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();

    let mut ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Binary,
        encoding_rs::UTF_8, io_tx, cache,
    ).await.unwrap();
    // 窄视口（<40 列）：行宽应为 8，而非配置的 16
    let mut vp = ViewPort::new(24, 30);
    assert_eq!(vp.bytes_per_line(&ed), 8, "窄终端 hex 行宽应降级为 8");

    ed.goto_byte(20);
    ed.move_cursor_lines(1, vp.bytes_per_line(&ed), &mut vp.line_index).await;
    assert_eq!(ed.cursor_byte, 28); // 20 + 8，与渲染行高一致
    // 8B/行：byte 28 → row=3, inner=4 → col=10+4*3=22（行内列保持）
    assert_eq!(vp.cursor_screen_pos(&ed, 8).await, Some((3, 22)));

    // 连续下移超出视口 → ensure_cursor_visible 按 8B/行滚动，保持可见
    for _ in 0..40 {
        ed.move_cursor_lines(1, vp.bytes_per_line(&ed), &mut vp.line_index).await;
        vp.ensure_cursor_visible(&ed).await;
        assert!(vp.cursor_screen_pos(&ed, 8).await.is_some(), "窄终端光标应始终在视口内");
    }

    // 正常宽度视口：行宽恢复为配置的 16
    let vp_wide = ViewPort::new(24, 80);
    assert_eq!(vp_wide.bytes_per_line(&ed), 16);
}

/// 核心修复验证：文本模式渲染的是【真实逻辑行】，而非旧的 80B/行估算块。
#[tokio::test]
async fn text_rows_follow_real_lines() {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use beditor::config::EditorConfig;
    use beditor::editor::Editor;
    use beditor::OpenMode;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    // 变长行：aaa / bb / ccccccc（不同长度，旧 80B 估算无法正确处理）
    std::fs::write(tmp.path(), b"aaa\nbb\nccccccc\n").unwrap();
    let cfg = Arc::new(EditorConfig::default());
    let (io_tx, io_rx) = mpsc::channel(128);
    let (ev_tx, _) = mpsc::channel(128);
    let cache = beditor::io_worker::spawn_io_workers(
        tmp.path().to_path_buf(), cfg.clone(), io_rx, ev_tx,
    ).await.unwrap();
    let ed = Editor::open(
        tmp.path().to_path_buf(), cfg, OpenMode::Text,
        encoding_rs::UTF_8, io_tx, cache,
    ).await.unwrap();
    let vp = ViewPort::new(24, 80);

    let rows = vp.text_rows(&ed).await;
    assert_eq!(rows[0], Some((0, b"aaa".to_vec())));
    assert_eq!(rows[1], Some((4, b"bb".to_vec())));
    assert_eq!(rows[2], Some((7, b"ccccccc".to_vec())));
    assert_eq!(rows[3], None, "超出文件末尾应返回 None");
    assert_eq!(rows[23], None);

    // 滚动一行：视口顶应到第 1 行（偏移 4）
    let mut vp2 = ViewPort::new(24, 80);
    vp2.scroll_lines(&ed, 1).await;
    assert_eq!(vp2.top_byte, 4, "按真实行滚动一行后顶部应为第 1 行");
}
