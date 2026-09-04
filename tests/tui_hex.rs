use beditor::tui::render_hex::*;
use beditor::tui::status_bar::*;

#[test]
fn format_offset_basic() {
    assert_eq!(format_offset(0), "00000000");
    assert_eq!(format_offset(255), "000000FF");
    assert_eq!(format_offset(0x12345678), "12345678");
}

#[test]
fn format_byte_hex_basic() {
    assert_eq!(format_byte_hex(0), "00");
    assert_eq!(format_byte_hex(0x41), "41");
    assert_eq!(format_byte_hex(0xFF), "FF");
}

#[test]
fn format_byte_ascii_printable() {
    assert_eq!(format_byte_ascii(0x41), 'A');
    assert_eq!(format_byte_ascii(0x7E), '~');
    assert_eq!(format_byte_ascii(0x20), ' ');
}

#[test]
fn format_byte_ascii_control() {
    assert_eq!(format_byte_ascii(0), '·');
    assert_eq!(format_byte_ascii(0x0A), '·');
    assert_eq!(format_byte_ascii(0x1F), '·');
    assert_eq!(format_byte_ascii(0x7F), '·');
}

#[test]
fn render_hex_row_full() {
    // "Hello W" = 7 bytes; index 5 = ' ' (0x20)
    let row = render_hex_row(0x100, b"Hello W", 16, None, &[]);
    assert_eq!(row.offset, "00000100");
    assert_eq!(row.hex_parts.len(), 16);
    assert_eq!(row.hex_parts[0].0, "48"); // 'H' = 0x48
    assert_eq!(row.hex_parts[5].0, "20"); // ' ' = 0x20
    assert_eq!(row.ascii_parts[0], ('H', false));
    // 超出数据长度的位置应该是空: 16 - 7 = 9 个空位
    assert_eq!(
        row.hex_parts[7..]
            .iter()
            .filter(|(h, _)| h == "  ")
            .count(),
        9
    );
}

#[test]
fn render_hex_row_cursor_highlight() {
    let row = render_hex_row(0, b"ABC", 16, Some(1), &[]);
    assert!(!row.hex_parts[0].1); // 'A' 不高亮
    assert!(row.hex_parts[1].1); // 'B' 高亮（cursor）
    assert!(!row.hex_parts[2].1); // 'C' 不高亮
}

#[test]
fn render_hex_row_search_highlight() {
    let row = render_hex_row(0, b"ABCDEF", 16, None, &[(1, 3)]); // 搜索匹配位置 1..3
    assert!(!row.hex_parts[0].1); // 'A' 不高亮
    assert!(row.hex_parts[1].1); // 'B' 高亮（搜索）
    assert!(row.hex_parts[2].1); // 'C' 高亮（搜索）
    assert!(!row.hex_parts[3].1); // 'D' 不高亮
}

#[test]
fn status_state_default() {
    let s = StatusState::default();
    assert_eq!(s.mode_name, "NORMAL");
    assert!(s.message.is_none());
    assert!(!s.in_command);
}

#[test]
fn mini_buffer_command() {
    let s = StatusState {
        in_command: true,
        command_buffer: "w".to_string(),
        ..Default::default()
    };
    let line = render_mini_buffer(&s);
    // 应该包含 ":w"
    assert!(!line.spans.is_empty());
}

#[test]
fn mini_buffer_error() {
    let s = StatusState {
        message: Some("保存失败".to_string()),
        message_is_error: true,
        ..Default::default()
    };
    let line = render_mini_buffer(&s);
    assert!(!line.spans.is_empty());
}
