use crate::editor::Editor;
use crate::config::OpenMode;
use crate::tui::status_bar::StatusState;
use crate::tui::viewport::ViewPort;

/// 编辑器模式
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Insert,
    Command,
    Search,
}

/// 命令解析结果
#[derive(Debug)]
pub enum CommandResult {
    None,
    Quit,
    ForceQuit,
    Save,
    SaveAs(String),
    SaveAndQuit,
    GotoByte(u64),
    SwitchMode(OpenMode),
    SetEncoding(String),
    Search(Vec<u8>),
    SearchNext,
    SearchPrev,
    Info,
    Help,
    /// 切换只读/可写（:ro / :rw）
    SetReadOnly(bool),
    Unknown(String),
}

/// 解析 :command 字符串
pub fn parse_command(input: &str) -> CommandResult {
    let input = input.trim();
    match input {
        "w" => CommandResult::Save,
        "q" => CommandResult::Quit,
        "q!" => CommandResult::ForceQuit,
        "wq" | "x" => CommandResult::SaveAndQuit,
        "h" | "help" => CommandResult::Help,
        "info" => CommandResult::Info,
        "ro" => CommandResult::SetReadOnly(true),
        "rw" | "e" => CommandResult::SetReadOnly(false),
        _ if input.starts_with("w ") => {
            let path = input[2..].trim().to_string();
            CommandResult::SaveAs(path)
        }
        _ if input.starts_with("goto ") => {
            let rest = input[5..].trim();
            let byte = if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16).unwrap_or(0)
            } else {
                rest.parse::<u64>().unwrap_or(0)
            };
            CommandResult::GotoByte(byte)
        }
        _ if input.starts_with("mode ") => {
            let mode = input[5..].trim();
            match mode {
                "text" => CommandResult::SwitchMode(OpenMode::Text),
                "hex" | "binary" => CommandResult::SwitchMode(OpenMode::Binary),
                _ => CommandResult::Unknown(format!("unknown mode: {mode}")),
            }
        }
        _ if input.starts_with("enc ") => {
            CommandResult::SetEncoding(input[4..].trim().to_string())
        }
        _ => CommandResult::Unknown(input.to_string()),
    }
}

/// NORMAL 模式按键动作
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NormalAction {
    None,
    Quit,
    EnterInsert,
    EnterSearch,
    Undo,
    Redo,
    SearchNext,
    SearchPrev,
}

/// 处理 NORMAL 模式按键，返回是否需要退出
pub async fn handle_normal_key(
    key: crossterm::event::KeyEvent,
    editor: &mut Editor,
    viewport: &mut ViewPort,
    status: &mut StatusState,
) -> NormalAction {
    use crossterm::event::{KeyCode, KeyModifiers};

    let code = key.code;
    let mods = key.modifiers;

    match code {
        KeyCode::Char('q') if mods.contains(KeyModifiers::CONTROL) => {
            NormalAction::Quit
        }
        KeyCode::Char(':') => {
            status.in_command = true;
            status.command_buffer.clear();
            NormalAction::None
        }
        KeyCode::Char('/') => {
            // 进入搜索模式
            status.mode_name = "SEARCH";
            status.in_command = true;
            status.command_buffer.clear();
            NormalAction::EnterSearch
        }
        KeyCode::Char('i') => {
            status.mode_name = "INSERT";
            NormalAction::EnterInsert
        }
        KeyCode::Char('a') => {
            editor.goto_byte(editor.cursor_byte + 1);
            viewport.ensure_cursor_visible(editor).await;
            status.mode_name = "INSERT";
            NormalAction::EnterInsert
        }
        KeyCode::Char('o') => {
            // 在文件末尾插入新行
            editor.goto_byte(editor.file_size);
            viewport.ensure_cursor_visible(editor).await;
            status.mode_name = "INSERT";
            NormalAction::EnterInsert
        }
        // Ctrl+U 必须在普通 u 之前匹配
        KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
            let lines = (viewport.visible_rows / 2) as i64;
            editor.move_cursor_lines(-lines, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.scroll_lines(editor, -lines).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::Char('u') => NormalAction::Undo,
        KeyCode::Char('R') if mods.contains(KeyModifiers::CONTROL) => NormalAction::Redo,
        KeyCode::Char('n') => NormalAction::SearchNext,
        KeyCode::Char('N') => NormalAction::SearchPrev,
        KeyCode::Char('G') => {
            editor.goto_byte(editor.file_size);
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::Char('g') => {
            editor.goto_byte(0);
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::Char('h') | KeyCode::Left => {
            editor.goto_byte(editor.cursor_byte.saturating_sub(1));
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::Char('l') | KeyCode::Right => {
            editor.goto_byte(editor.cursor_byte + 1);
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            editor.move_cursor_lines(-1, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::Char('j') | KeyCode::Down => {
            editor.move_cursor_lines(1, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::PageDown => {
            let lines = viewport.visible_rows as i64;
            editor.move_cursor_lines(lines, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.scroll_page(editor, 1).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::PageUp => {
            let lines = -(viewport.visible_rows as i64);
            editor.move_cursor_lines(lines, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.scroll_page(editor, -1).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        // f 下翻页 / b 上翻页（与 PageDown/PageUp 等价）
        KeyCode::Char('f') => {
            let lines = viewport.visible_rows as i64;
            editor.move_cursor_lines(lines, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.scroll_page(editor, 1).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        KeyCode::Char('b') => {
            let lines = -(viewport.visible_rows as i64);
            editor.move_cursor_lines(lines, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.scroll_page(editor, -1).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        // Esc 退出程序（经 try_quit：有未保存修改时提示而非直接退出）
        KeyCode::Esc => NormalAction::Quit,
        KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => {
            let lines = (viewport.visible_rows / 2) as i64;
            editor.move_cursor_lines(lines, viewport.bytes_per_line(editor), &mut viewport.line_index).await;
            viewport.scroll_lines(editor, lines).await;
            viewport.ensure_cursor_visible(editor).await;
            NormalAction::None
        }
        _ => NormalAction::None,
    }
}

/// INSERT 模式按键动作
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InsertAction {
    InsertByte(u8),
    Backspace,
    ExitToNormal,
}

/// 处理 INSERT 模式按键
pub fn handle_insert_key(
    key: crossterm::event::KeyEvent,
) -> Option<InsertAction> {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => Some(InsertAction::ExitToNormal),
        KeyCode::Char(c) => Some(InsertAction::InsertByte(c as u8)),
        KeyCode::Enter => Some(InsertAction::InsertByte(b'\n')),
        KeyCode::Backspace => Some(InsertAction::Backspace),
        _ => None,
    }
}
