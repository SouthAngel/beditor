use ratatui::text::{Line, Span};
use crate::config::EditorConfig;
use crate::encoding;

/// 将一行字节渲染为 ratatui Line
pub fn render_text_line(
    bytes: &[u8],
    encoding: &'static encoding_rs::Encoding,
    config: &EditorConfig,
    _cursor_byte_in_line: Option<usize>,
    _search_ranges: &[(usize, usize)], // 搜索高亮范围 (start, end)
) -> Line<'static> {
    let decoded = encoding::decode(bytes, encoding);
    // Tab 展开
    let expanded = expand_tabs(&decoded, config.tab_width as usize);

    // 简化：先不做搜索高亮和光标高亮，只返回纯文本行
    // 后续可以精确处理
    if expanded.is_empty() {
        return Line::from(Span::raw(" "));
    }

    Line::from(Span::raw(expanded))
}

/// Tab 展开为空格
pub fn expand_tabs(s: &str, tab_width: usize) -> String {
    let mut result = String::with_capacity(s.len() * 2);
    let mut col = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = tab_width - (col % tab_width);
            result.extend(std::iter::repeat(' ').take(spaces));
            col += spaces;
        } else {
            result.push(ch);
            col += 1;
        }
    }
    result
}

/// 截断行到可见列数
pub fn truncate_line(line: &str, max_cols: usize) -> String {
    if line.len() <= max_cols {
        line.to_string()
    } else {
        line.chars().take(max_cols).collect()
    }
}
