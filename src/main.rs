use std::path::PathBuf;
use std::sync::Arc;
use clap::{Parser, ValueEnum};
use beditor::config::{EditorConfig, OpenMode};
use beditor::editor::Editor;
use beditor::encoding;
use beditor::io_worker::spawn_io_workers;
use beditor::message::{IoEvent, IoRequest, Direction};
use beditor::tui;
use beditor::tui::status_bar::StatusState;
use beditor::tui::viewport::ViewPort;
use beditor::command::{parse_command, handle_normal_key, handle_insert_key, InputMode, NormalAction, InsertAction};

#[derive(Parser)]
#[command(name = "beditor", about = "通用大文件终端编辑器")]
struct Cli {
    /// 要打开的文件路径
    file: PathBuf,

    /// 初始打开模式
    #[arg(short = 'm', long = "mode", value_enum, default_value = "auto")]
    mode: ModeArg,

    /// 块大小 (KB)
    #[arg(long = "block-size", default_value = "256")]
    block_size_kb: u32,

    /// 内存使用比率 (0.05 ~ 0.8)
    #[arg(long = "mem-ratio", default_value = "0.3")]
    mem_ratio: f32,

    /// 以可写模式打开（默认只读）
    #[arg(long = "rw")]
    rw: bool,
}

#[derive(Clone, ValueEnum)]
enum ModeArg {
    Auto,
    Text,
    Hex,
}

impl From<ModeArg> for OpenMode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::Auto => OpenMode::Auto,
            ModeArg::Text => OpenMode::Text,
            ModeArg::Hex => OpenMode::Binary,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mode: OpenMode = cli.mode.into();
    let config = Arc::new(EditorConfig::from_cli(
        Some(cli.block_size_kb),
        Some(cli.mem_ratio),
        Some(mode),
    )?);

    // 初始化 TUI
    let mut terminal = tui::init_tui()?;

    // 边界加固：检查上次保存崩溃遗留的临时文件
    let tmp_path = cli.file.with_extension("beditor-tmp");
    if tmp_path.exists() {
        eprintln!("警告: 检测到上次保存崩溃遗留的临时文件: {}", tmp_path.display());
        eprintln!("  请手动检查该文件是否包含未保存的数据，确认后删除。");
    }

    // 创建通道
    let (io_tx, io_rx) = tokio::sync::mpsc::channel::<IoRequest>(128);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<IoEvent>(256);

    // 启动 IO workers
    let cache = spawn_io_workers(
        cli.file.clone(), config.clone(), io_rx, event_tx,
    ).await?;

    // 读文件头 → 编码检测 → 模式决策
    let header_bytes = {
        let (tx, rx) = tokio::sync::oneshot::channel();
        io_tx.send(IoRequest::ReadFileHeader { max_bytes: 64 * 1024, reply: tx }).await?;
        rx.await?
    };
    let (detected_mode, detected_encoding) = encoding::detect_mode(&header_bytes, &cli.file, config.default_mode);

    // 创建 Editor
    let mut editor = Editor::open(
        cli.file.clone(), config, detected_mode, detected_encoding,
        io_tx.clone(), cache,
    ).await?;

    let mut viewport = ViewPort::new(24, 80);
    let mut status = StatusState::default();
    // 默认只读打开；--rw 才可写
    editor.read_only = !cli.rw;
    if editor.read_only {
        status.message = Some("只读模式（:rw 切换为可写）".into());
        status.message_is_error = false;
    }
    // 打开时发现上次会话未合并的 WAL 增量（崩溃/未 :wq 退出残留）
    if editor.wal_pending() {
        status.message = Some("检测到未合并的 WAL 数据，已自动恢复。:w 保存 / :wq 合并退出".into());
        status.message_is_error = true;
    }
    let mut input_mode = InputMode::Normal;

    // 主事件循环
    let mut should_quit = false;
    // 记录最近一次 PrefetchRange，避免每帧重复请求
    let mut last_prefetch_range: Option<(u64, u64)> = None;

    while !should_quit {
        // 视口尺寸随终端大小更新（标题/输入/状态栏各占 1 行）
        let term_size = terminal.size()?;
        let view_rows_count = (term_size.height as usize).saturating_sub(3).max(1);
        let view_cols = term_size.width as usize;
        viewport.resize(view_rows_count, view_cols);

        // --- 修复: 首帧和每次视口变化时，向 IO worker 请求视口覆盖范围的块预取
        prefetch_viewport_if_needed(
            &editor, &viewport, &io_tx, &mut last_prefetch_range,
        ).await;

        // 异步预计算视口行内容与光标屏幕位置（draw 闭包内无法 await）
        let bytes_per_row = viewport.bytes_per_line(&editor) as usize;
        let view_rows: Vec<Option<(u64, Vec<u8>)>> = match editor.mode {
            OpenMode::Text | OpenMode::Auto => viewport.text_rows(&editor).await,
            OpenMode::Binary => viewport.hex_rows(&editor, bytes_per_row),
        };
        let cursor_screen = viewport.cursor_screen_pos(&editor, bytes_per_row).await;

        // 渲染
        tui::render(
            &mut terminal, &editor, &viewport, &status,
            &view_rows, bytes_per_row, cursor_screen,
        )?;

        // select! 两路复用：定时检查终端输入 + IO 事件
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                // 定时检查终端输入（非阻塞 poll）
                if let Ok(true) = crossterm::event::poll(std::time::Duration::from_millis(0)) {
                    if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                        // 仅处理 Press 事件；过滤 Release/Repeat，避免一次按键重复输入
                        if key.kind != crossterm::event::KeyEventKind::Press {
                            continue;
                        }
                        // 边界加固：Ctrl+C 提示而非直接退出
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                            && key.code == crossterm::event::KeyCode::Char('c')
                        {
                            try_quit(&mut editor, &mut should_quit, &mut status).await;
                            continue;
                        }
                        match input_mode {
                            InputMode::Normal => {
                                let action = handle_normal_key(key, &mut editor, &mut viewport, &mut status).await;
                                match action {
                                    NormalAction::Quit => {
                                        try_quit(&mut editor, &mut should_quit, &mut status).await;
                                    }
                                    NormalAction::EnterInsert => {
                                        if editor.read_only {
                                            status.mode_name = "NORMAL";
                                            status.message = Some("只读模式，:rw 切换为可写".into());
                                            status.message_is_error = true;
                                        } else {
                                            input_mode = InputMode::Insert;
                                        }
                                    }
                                    NormalAction::EnterSearch => {
                                        input_mode = InputMode::Search;
                                    }
                                    NormalAction::Undo => {
                                        if editor.read_only {
                                            status.message = Some("只读模式，:rw 切换为可写".into());
                                            status.message_is_error = true;
                                        } else {
                                            match editor.undo().await {
                                                Ok(()) => viewport.ensure_cursor_visible(&editor).await,
                                                Err(e) => {
                                                    status.message = Some(format!("{}", e));
                                                    status.message_is_error = true;
                                                }
                                            }
                                        }
                                    }
                                    NormalAction::Redo => {
                                        if editor.read_only {
                                            status.message = Some("只读模式，:rw 切换为可写".into());
                                            status.message_is_error = true;
                                        } else {
                                            match editor.redo().await {
                                                Ok(()) => viewport.ensure_cursor_visible(&editor).await,
                                                Err(e) => {
                                                    status.message = Some(format!("{}", e));
                                                    status.message_is_error = true;
                                                }
                                            }
                                        }
                                    }
                                    NormalAction::SearchNext => {
                                        if let Some(query) = editor.last_search.clone() {
                                            match editor.search_literal(&query, Direction::Forward, 1).await {
                                                Ok(r) if !r.matches.is_empty() => {
                                                    editor.goto_byte(r.matches[0]);
                                                    viewport.ensure_cursor_visible(&editor).await;
                                                }
                                                _ => {
                                                    status.message = Some("无匹配".into());
                                                    status.message_is_error = true;
                                                }
                                            }
                                        } else {
                                            status.message = Some("暂无搜索历史，按 / 开始搜索".into());
                                            status.message_is_error = true;
                                        }
                                    }
                                    NormalAction::SearchPrev => {
                                        if let Some(query) = editor.last_search.clone() {
                                            match editor.search_literal(&query, Direction::Backward, 1).await {
                                                Ok(r) if !r.matches.is_empty() => {
                                                    editor.goto_byte(r.matches[0]);
                                                    viewport.ensure_cursor_visible(&editor).await;
                                                }
                                                _ => {
                                                    status.message = Some("无匹配".into());
                                                    status.message_is_error = true;
                                                }
                                            }
                                        } else {
                                            status.message = Some("暂无搜索历史，按 / 开始搜索".into());
                                            status.message_is_error = true;
                                        }
                                    }
                                    NormalAction::None => {}
                                }
                            }
                            InputMode::Insert => {
                                if let Some(action) = handle_insert_key(key) {
                                    match action {
                                        InsertAction::ExitToNormal => {
                                            input_mode = InputMode::Normal;
                                            status.mode_name = "NORMAL";
                                            viewport.ensure_cursor_visible(&editor).await;
                                        }
                                        InsertAction::InsertByte(byte) => {
                                            if editor.read_only {
                                                status.message = Some("只读模式，Esc 退出后 :rw 切换为可写".into());
                                                status.message_is_error = true;
                                            } else if let Err(e) = editor.insert_bytes_at_cursor(&[byte]).await {
                                                status.message = Some(format!("{}", e));
                                                status.message_is_error = true;
                                            }
                                            viewport.ensure_cursor_visible(&editor).await;
                                        }
                                        InsertAction::Backspace => {
                                            if editor.read_only {
                                                status.message = Some("只读模式，Esc 退出后 :rw 切换为可写".into());
                                                status.message_is_error = true;
                                            } else if editor.cursor_byte > 0 {
                                                editor.goto_byte(editor.cursor_byte - 1);
                                                if let Err(e) = editor.delete_bytes_at_cursor_forward(1).await {
                                                    status.message = Some(format!("{}", e));
                                                    status.message_is_error = true;
                                                }
                                            }
                                            viewport.ensure_cursor_visible(&editor).await;
                                        }
                                    }
                                }
                            }
                            InputMode::Command => {
                                match key.code {
                                    crossterm::event::KeyCode::Enter => {
                                        let cmd = parse_command(&status.command_buffer);
                                        status.command_buffer.clear();
                                        status.in_command = false;
                                        input_mode = InputMode::Normal;
                                        status.mode_name = "NORMAL";
                                        handle_command_result(cmd, &mut editor, &mut viewport, &mut status, &mut should_quit).await;
                                    }
                                    crossterm::event::KeyCode::Char(c) => {
                                        status.command_buffer.push(c);
                                    }
                                    crossterm::event::KeyCode::Backspace => {
                                        status.command_buffer.pop();
                                    }
                                    crossterm::event::KeyCode::Esc => {
                                        status.in_command = false;
                                        status.command_buffer.clear();
                                        input_mode = InputMode::Normal;
                                        status.mode_name = "NORMAL";
                                    }
                                    _ => {}
                                }
                            }
                            InputMode::Search => {
                                match key.code {
                                    crossterm::event::KeyCode::Enter => {
                                        let query = status.command_buffer.clone().into_bytes();
                                        status.command_buffer.clear();
                                        status.in_command = false;
                                        input_mode = InputMode::Normal;
                                        status.mode_name = "NORMAL";
                                        if !query.is_empty() {
                                            match editor.search_literal(&query, Direction::Forward, 100).await {
                                                Ok(r) if !r.matches.is_empty() => {
                                                    editor.goto_byte(r.matches[0]);
                                                    viewport.ensure_cursor_visible(&editor).await;
                                                    status.message = Some(format!("找到 {} 个匹配", r.matches.len()));
                                                }
                                                _ => {
                                                    status.message = Some("无匹配".into());
                                                }
                                            }
                                        }
                                    }
                                    crossterm::event::KeyCode::Char(c) => {
                                        status.command_buffer.push(c);
                                    }
                                    crossterm::event::KeyCode::Backspace => {
                                        status.command_buffer.pop();
                                    }
                                    crossterm::event::KeyCode::Esc => {
                                        status.in_command = false;
                                        status.command_buffer.clear();
                                        input_mode = InputMode::Normal;
                                        status.mode_name = "NORMAL";
                                    }
                                    _ => {}
                                }
                            }
                        }
                        // 处理 : 进入命令模式
                        if input_mode == InputMode::Normal && status.in_command {
                            input_mode = InputMode::Command;
                        }
                    }
                }
            }
            // IO 事件
            Some(event) = event_rx.recv() => {
                match event {
                    IoEvent::Error(e) => {
                        status.message = Some(format!("IO错误: {e}"));
                        status.message_is_error = true;
                    }
                    IoEvent::SaveProgress { ratio } => {
                        status.message = Some(format!("保存中... {:.0}%", ratio * 100.0));
                        status.message_is_error = false;
                    }
                    IoEvent::BlockFlushed { block_id } => {
                        tracing::debug!(block_id, "block flushed");
                    }
                    IoEvent::PrefetchCompleted { .. } => {}
                }
            }
        }
    }

    // 清理
    tui::restore_tui(&mut terminal)?;
    Ok(())
}

/// 尝试退出：
/// - 有内存脏块 → 警告（需 :w 或 :q!）
/// - 无脏块但有未合并 WAL 增量 → 合并到基础文件后干净退出
/// - 否则直接退出
async fn try_quit(editor: &mut Editor, should_quit: &mut bool, status: &mut StatusState) {
    if !editor.cache.dirty_ids().is_empty() {
        status.message = Some("有未保存修改，使用 :q! 强制退出 或 :w 保存".into());
        status.message_is_error = true;
    } else if editor.wal_pending() {
        // 无内存脏块但有未合并 WAL 增量：合并到基础文件后干净退出（save_as 内部会清 WAL）
        match editor.save_as(editor.file_path.clone()).await {
            Ok(()) => *should_quit = true,
            Err(e) => {
                status.message = Some(format!("合并 WAL 失败: {e}"));
                status.message_is_error = true;
            }
        }
    } else {
        *should_quit = true;
    }
}

async fn handle_command_result(
    cmd: beditor::command::CommandResult,
    editor: &mut Editor,
    viewport: &mut ViewPort,
    status: &mut StatusState,
    should_quit: &mut bool,
) {
    use beditor::command::CommandResult;
    match cmd {
        CommandResult::None => {}
        CommandResult::Quit => {
            try_quit(editor, should_quit, status).await;
        }
        CommandResult::ForceQuit => {
            *should_quit = true;
        }
        CommandResult::Save => {
            if editor.read_only {
                status.message = Some("只读模式，:rw 切换为可写后再保存".into());
                status.message_is_error = true;
                return;
            }
            // :w 改为增量保存（脏块追加到 WAL），大文件秒存；退出时再合并
            match editor.save_incremental().await {
                Ok(_) => {
                    status.message = Some("已保存（WAL）".into());
                    status.message_is_error = false;
                }
                Err(e) => {
                    status.message = Some(format!("保存失败: {e}"));
                    status.message_is_error = true;
                }
            }
        }
        CommandResult::SaveAs(path) => {
            if editor.read_only {
                status.message = Some("只读模式，:rw 切换为可写后再保存".into());
                status.message_is_error = true;
                return;
            }
            match editor.save_as(PathBuf::from(path)).await {
                Ok(()) => {
                    status.message = Some("已保存".into());
                    status.message_is_error = false;
                }
                Err(e) => {
                    status.message = Some(format!("保存失败: {e}"));
                    status.message_is_error = true;
                }
            }
        }
        CommandResult::SaveAndQuit => {
            if editor.read_only {
                status.message = Some("只读模式，:rw 切换为可写后再保存退出".into());
                status.message_is_error = true;
                return;
            }
            // 合并（把 WAL 写入基础文件）并退出；save_as 内部会清 WAL
            match editor.save_as(editor.file_path.clone()).await {
                Ok(()) => *should_quit = true,
                Err(e) => {
                    status.message = Some(format!("保存失败: {e}"));
                    status.message_is_error = true;
                }
            }
        }
        CommandResult::SetReadOnly(ro) => {
            editor.read_only = ro;
            if ro {
                status.message = Some("已切换为只读".into());
                status.message_is_error = false;
            } else {
                // 切回可写需要 WAL 支撑增量持久化；惰性创建临时文件
                match editor.ensure_wal().await {
                    Ok(()) => {
                        status.message = Some("已切换为可写".into());
                        status.message_is_error = false;
                    }
                    Err(e) => {
                        status.message = Some(format!("切换可写失败: {e}"));
                        status.message_is_error = true;
                        editor.read_only = true; // 回退，避免无 WAL 时编辑丢数据
                    }
                }
            }
        }
        CommandResult::GotoByte(byte) => {
            editor.goto_byte(byte);
            viewport.ensure_cursor_visible(editor).await;
        }
        CommandResult::SwitchMode(mode) => {
            editor.mode = mode;
            viewport.ensure_cursor_visible(editor).await;
        }
        CommandResult::SetEncoding(name) => {
            let enc = match name.as_str() {
                "utf-8" | "utf8" => encoding_rs::UTF_8,
                "gbk" => encoding_rs::GBK,
                "gb18030" => encoding_rs::GB18030,
                _ => {
                    status.message = Some(format!("未知编码: {name}"));
                    status.message_is_error = true;
                    return;
                }
            };
            editor.text_encoding = enc;
            status.message = Some(format!("编码切换为: {name}"));
            status.message_is_error = false;
        }
        CommandResult::Search(query) => {
            match editor.search_literal(&query, Direction::Forward, 100).await {
                Ok(r) if !r.matches.is_empty() => {
                    editor.goto_byte(r.matches[0]);
                    viewport.ensure_cursor_visible(editor).await;
                }
                _ => {
                    status.message = Some("无匹配".into());
                }
            }
        }
        CommandResult::SearchNext | CommandResult::SearchPrev => {
            // 在主循环中处理
        }
        CommandResult::Info => {
            let stats = editor.cache.stats();
            status.message = Some(format!(
                "文件: {} | 块: {} | 缓存: {}/{} | 命中: {} | 遗漏: {} | 淘汰: {}",
                editor.file_size,
                editor.block_count,
                stats.size_blocks,
                stats.capacity_blocks,
                stats.hits,
                stats.misses,
                stats.evictions,
            ));
            status.message_is_error = false;
        }
        CommandResult::Help => {
            status.message = Some(":w 保存 | :q 退出 | :q! 强制退出 | :wq 保存退出 | :goto <offset> | :mode text/hex | :enc <name> | i 插入 | u 撤销 | Ctrl+R 重做 | f 下页 | b 上页 | Esc 退出".into());
            status.message_is_error = false;
        }
        CommandResult::Unknown(s) => {
            status.message = Some(format!("未知命令: {s}"));
            status.message_is_error = true;
        }
    }
}

/// 计算当前视口覆盖的字节范围，换算成 [start_block, end_block)（左闭右开）
/// 并合并 config.prefetch_behind / prefetch_ahead 窗口。若与上次范围不同，
/// 就发送一次 `IoRequest::PrefetchRange` 给 IO workers，让它们把块异步装到 cache。
///
/// 这是修复「打开初始化不显示内容」的关键：首帧渲染前主动把整屏覆盖的块
/// 预先加载，viewport.text_row / hex_row 从 cache 拿到字节才会真正显示出来。
async fn prefetch_viewport_if_needed(
    editor: &Editor,
    viewport: &ViewPort,
    io_tx: &tokio::sync::mpsc::Sender<IoRequest>,
    last: &mut Option<(u64, u64)>,
) {
    if editor.file_size == 0 || editor.block_count == 0 {
        return;
    }

    // 估算当前视口末行结束的字节偏移（与视口行高保持一致）
    let approx_bytes_per_row = viewport.bytes_per_line(editor);

    let bs = editor.config.block_size as u64;
    let visible_bytes = viewport.visible_rows as u64 * approx_bytes_per_row;
    let top = viewport.top_byte.min(editor.file_size.saturating_sub(1));
    let bottom = (top + visible_bytes).min(editor.file_size);

    let behind = (editor.config.prefetch_behind as u64).saturating_mul(bs);
    let ahead = (editor.config.prefetch_ahead as u64).saturating_mul(bs);

    let range_start_byte = top.saturating_sub(behind);
    let range_end_byte = (bottom + ahead).min(editor.file_size);

    let start_block = range_start_byte / bs;
    // end_block 是 exclusive（PrefetchRange 使用 [start, end) 左闭右开）
    let end_block = range_end_byte.div_ceil(bs).min(editor.block_count);
    let end_block = end_block.max(start_block + 1);

    let range = (start_block, end_block);
    if last.as_ref() == Some(&range) {
        return;
    }
    *last = Some(range);

    // PrefetchRange 是 fire-and-forget：workers 异步加载块并写入 cache，
    // 下一轮渲染循环的 viewport.text_row/hex_row 就能命中 cache。
    let _ = io_tx.send(IoRequest::PrefetchRange {
        start_block: range.0,
        end_block: range.1,
    }).await;
}
