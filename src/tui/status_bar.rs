use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use crate::cache::CacheStats;
use crate::editor::Editor;

/// 状态栏数据
pub struct StatusState {
    pub mode_name: &'static str,
    pub message: Option<String>,
    pub message_is_error: bool,
    pub command_buffer: String,
    pub in_command: bool,
}

impl Default for StatusState {
    fn default() -> Self {
        Self {
            mode_name: "NORMAL",
            message: None,
            message_is_error: false,
            command_buffer: String::new(),
            in_command: false,
        }
    }
}

/// 渲染状态栏
pub fn render_status_bar(editor: &Editor, stats: &CacheStats, status: &StatusState) -> Line<'static> {
    let mode_str = match editor.mode {
        crate::config::OpenMode::Text => "Text",
        crate::config::OpenMode::Binary => "Hex",
        crate::config::OpenMode::Auto => "Auto",
    };
    let rw_str = if editor.read_only { "RO" } else { "RW" };

    let hit_rate = if stats.hits + stats.misses > 0 {
        stats.hits as f64 / (stats.hits + stats.misses) as f64 * 100.0
    } else {
        0.0
    };

    let line = format!(
        "[{}] [{}] [{}] Pos {}/{} Block {}/{} Cache {}/{} Hit {:.0}% Enc {}",
        status.mode_name,
        mode_str,
        rw_str,
        editor.cursor_byte,
        editor.file_size,
        editor.cursor_byte / editor.config.block_size as u64,
        editor.block_count,
        stats.size_blocks,
        stats.capacity_blocks,
        hit_rate,
        editor.text_encoding.name(),
    );

    Line::from(Span::styled(line, Style::default().fg(Color::Cyan)))
}

/// 渲染 MiniBuffer（命令行/消息区）
pub fn render_mini_buffer(status: &StatusState) -> Line<'static> {
    if status.in_command {
        Line::from(Span::raw(format!(":{}", status.command_buffer)))
    } else if let Some(msg) = &status.message {
        let style = if status.message_is_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };
        Line::from(Span::styled(msg.clone(), style))
    } else {
        Line::from(Span::raw(""))
    }
}
