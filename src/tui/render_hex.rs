use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// 格式化偏移为 8 位十六进制字符串
pub fn format_offset(offset: u64) -> String {
    format!("{:08X}", offset)
}

/// 格式化单个字节为两位十六进制
pub fn format_byte_hex(byte: u8) -> String {
    format!("{:02X}", byte)
}

/// 格式化字节为 ASCII 字符（控制字符显示为点号）
pub fn format_byte_ascii(byte: u8) -> char {
    if (0x20..=0x7E).contains(&byte) {
        byte as char
    } else {
        '·'
    }
}

/// 一行 hex 渲染结果
pub struct HexRow {
    pub offset: String,
    pub hex_parts: Vec<(String, bool)>,
    pub ascii_parts: Vec<(char, bool)>,
}

impl HexRow {
    /// 转换为 ratatui Line（用于直接渲染）
    /// 格式: "00000000  48 65 6C 6C 6F 20 57 6F  Hello Wo"
    pub fn to_line(&self) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();

        // 偏移列
        spans.push(Span::raw(format!("{}  ", self.offset)));

        // Hex 列
        for (hex, highlighted) in &self.hex_parts {
            if *highlighted {
                spans.push(Span::styled(
                    format!("{} ", hex),
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            } else {
                spans.push(Span::raw(format!("{} ", hex)));
            }
        }

        // 间距
        spans.push(Span::raw(" "));

        // ASCII 列
        for (ch, highlighted) in &self.ascii_parts {
            if *highlighted {
                spans.push(Span::styled(
                    ch.to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            } else {
                spans.push(Span::raw(ch.to_string()));
            }
        }

        Line::from(spans)
    }
}

/// 渲染一行 hex 数据
/// 返回 HexRow
/// offset_string: 8位hex偏移
/// hex_parts: 每个字节的 hex 表示 + 是否高亮
/// ascii_parts: 每个字节的 ascii 字符 + 是否高亮
pub fn render_hex_row(
    offset: u64,
    bytes: &[u8],
    bytes_per_row: usize,
    cursor_byte_index: Option<usize>,
    search_ranges: &[(usize, usize)],
) -> HexRow {
    let offset_str = format_offset(offset);
    let mut hex_parts: Vec<(String, bool)> = Vec::new();
    let mut ascii_parts: Vec<(char, bool)> = Vec::new();

    for i in 0..bytes_per_row {
        if i < bytes.len() {
            let byte = bytes[i];
            let is_cursor = cursor_byte_index == Some(i);
            let is_search = search_ranges.iter().any(|(s, e)| i >= *s && i < *e);
            let highlighted = is_cursor || is_search;
            hex_parts.push((format_byte_hex(byte), highlighted));
            ascii_parts.push((format_byte_ascii(byte), highlighted));
        } else {
            hex_parts.push(("  ".to_string(), false));
            ascii_parts.push((' ', false));
        }
    }

    HexRow {
        offset: offset_str,
        hex_parts,
        ascii_parts,
    }
}
