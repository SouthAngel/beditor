pub mod viewport;
pub mod render_text;
pub mod render_hex;
pub mod status_bar;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Terminal;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use std::io::Stdout;
use crate::editor::Editor;
use crate::config::OpenMode;
use crate::tui::status_bar::{render_status_bar, render_mini_buffer, StatusState};
use crate::tui::viewport::ViewPort;

pub type BeditorTerminal = Terminal<CrosstermBackend<Stdout>>;

pub fn init_tui() -> std::io::Result<BeditorTerminal> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_tui(terminal: &mut BeditorTerminal) -> std::io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// 主渲染函数。
///
/// 行内容（view_rows）与光标屏幕位置（cursor_screen）由主循环异步预计算，
/// 这里只做同步绘制（draw 闭包内无法 await 行索引的块加载）。
pub fn render(
    terminal: &mut BeditorTerminal,
    editor: &Editor,
    viewport: &ViewPort,
    status: &StatusState,
    view_rows: &[Option<(u64, Vec<u8>)>],
    bytes_per_row: usize,
    cursor_screen: Option<(usize, usize)>,
) -> std::io::Result<()> {
    terminal.draw(|frame| {
        let area = frame.size();

        // 布局：标题栏(1) + 视口(fill) + MiniBuffer(1) + 状态栏(1)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),  // Title bar
                Constraint::Min(1),    // Viewport
                Constraint::Length(1), // MiniBuffer
                Constraint::Length(1), // StatusBar
            ])
            .split(area);

        // 标题栏
        let title = format!(
            " beditor │ {} │ {} │ {} ",
            editor.file_path.display(),
            format_size(editor.file_size),
            editor.text_encoding.name(),
        );
        frame.render_widget(
            Paragraph::new(title)
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            chunks[0],
        );

        // 视口：根据模式把预计算的行内容转成 Line
        let viewport_area = chunks[1];
        let height = viewport_area.height as usize;
        // 清空视口区域，避免短行/EOF 空行/模式切换后右侧残留上一帧的字符
        frame.render_widget(Clear, viewport_area);
        match editor.mode {
            OpenMode::Text | OpenMode::Auto => {
                let mut lines: Vec<Line> = Vec::with_capacity(height);
                for item in view_rows.iter().take(height) {
                    if let Some((_offset, bytes)) = item {
                        lines.push(crate::tui::render_text::render_text_line(
                            bytes,
                            editor.text_encoding,
                            &editor.config,
                            None,
                            &[],
                        ));
                    } else {
                        lines.push(Line::from(Span::raw("~")));
                    }
                }
                frame.render_widget(Paragraph::new(lines), viewport_area);
            }
            OpenMode::Binary => {
                // 光标所在字节在当前视口中的行内索引（用于 hex 视觉高亮）
                let cursor_inner = if editor.cursor_byte >= viewport.top_byte {
                    let rel = editor.cursor_byte - viewport.top_byte;
                    let bpr = bytes_per_row as u64;
                    let row = rel / bpr;
                    let inner = rel % bpr;
                    if row < height as u64 {
                        Some((row as usize, inner as usize))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let mut lines: Vec<Line> = Vec::with_capacity(height);
                for (row, item) in view_rows.iter().take(height).enumerate() {
                    if let Some((offset, bytes)) = item {
                        let cursor_idx = cursor_inner
                            .filter(|(r, _)| *r == row)
                            .map(|(_, i)| i);
                        lines.push(crate::tui::render_hex::render_hex_row(
                            *offset, bytes, bytes_per_row, cursor_idx, &[],
                        ).to_line());
                    } else {
                        lines.push(Line::from(Span::raw("")));
                    }
                }
                frame.render_widget(Paragraph::new(lines), viewport_area);
            }
        }

        // MiniBuffer
        let mb_line = render_mini_buffer(status);
        frame.render_widget(Paragraph::new(mb_line), chunks[2]);

        // 状态栏
        let stats = editor.cache.stats();
        let sb_line = render_status_bar(editor, &stats, status);
        frame.render_widget(Paragraph::new(sb_line), chunks[3]);

        // 定位硬件光标到编辑器光标位置（否则光标在渲染期间不可见/错位）
        if let Some((row, col)) = cursor_screen {
            if row < height {
                let x = (viewport_area.x as usize + col) as u16;
                let y = (viewport_area.y as usize + row) as u16;
                frame.set_cursor(x, y);
            }
        }
    })?;
    Ok(())
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
