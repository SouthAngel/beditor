use std::collections::hash_map::Entry;
use std::collections::HashMap;
use crate::cache::BlockSnapshot;
use crate::config::OpenMode;
use crate::editor::Editor;
use crate::line_index::LineIndex;

/// 视口：控制当前显示的文件区域
pub struct ViewPort {
    /// 视口顶部对应的字节偏移（文本模式始终对齐行首）
    pub top_byte: u64,
    /// 可见行数
    pub visible_rows: usize,
    /// 可见列数
    pub visible_cols: usize,
    /// 文本模式行索引（行号 ↔ 字节偏移）
    pub line_index: LineIndex,
}

impl ViewPort {
    pub fn new(visible_rows: usize, visible_cols: usize) -> Self {
        Self {
            top_byte: 0,
            visible_rows,
            visible_cols,
            line_index: LineIndex::new(),
        }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.visible_rows = rows;
        self.visible_cols = cols;
    }

    /// 当前生效的每行字节数（与渲染保持一致）：
    /// - Binary：visible_cols < 40（窄终端）时降级为 8，否则 config.hex_bytes_per_row
    /// - Text/Auto：80 字节/行（估算，仅用于 prefetch 等粗略估算）
    pub fn bytes_per_line(&self, editor: &Editor) -> u64 {
        match editor.mode {
            OpenMode::Binary => {
                if self.visible_cols < 40 {
                    8
                } else {
                    editor.config.hex_bytes_per_row as u64
                }
            }
            _ => 80,
        }
    }

    /// 视口顶部所在行号（文本模式）
    async fn top_line(&mut self, editor: &Editor) -> u64 {
        self.line_index.offset_to_line(editor, self.top_byte).await.0
    }

    /// 滚动 N 行（正=向下，负=向上）。
    /// 文本模式按真实逻辑行滚动；Hex 模式按 bytes_per_line 字节滚动。
    pub async fn scroll_lines(&mut self, editor: &Editor, lines: i64) {
        match editor.mode {
            OpenMode::Binary => {
                let delta = lines * self.bytes_per_line(editor) as i64;
                let new_top = (self.top_byte as i64 + delta).max(0) as u64;
                self.top_byte = new_top.min(editor.file_size);
            }
            _ => {
                let total = self.line_index.total_lines(editor).await;
                let top_line = self.top_line(editor).await;
                let new_line = ((top_line as i64 + lines).max(0) as u64)
                    .min(total.saturating_sub(1));
                self.top_byte = self.line_index.line_start(editor, new_line).await;
            }
        }
    }

    /// 滚动 1 页（文本模式 = visible_rows 个逻辑行）
    pub async fn scroll_page(&mut self, editor: &Editor, pages: i64) {
        self.scroll_lines(editor, pages * self.visible_rows as i64).await;
    }

    /// 把编辑器光标(cursor_byte)映射到视口内的屏幕坐标 (row, col)。
    ///
    /// 返回 None 表示光标不在当前视口内（正常情况下 ensure_cursor_visible
    /// 已保证光标在视口内）。
    ///
    /// - 文本模式：row = 光标逻辑行 - 视口顶行；col = 行内字节列（近似屏幕列）
    /// - Hex 模式：行宽经 bytes_per_line；列位置映射到 hex 列的起始字符
    ///   （偏移列 8 字符 + 2 空格 + i*3）。
    pub async fn cursor_screen_pos(
        &mut self,
        editor: &Editor,
        bytes_per_row: usize,
    ) -> Option<(usize, usize)> {
        match editor.mode {
            OpenMode::Binary => {
                if editor.cursor_byte < self.top_byte {
                    return None;
                }
                let rel = editor.cursor_byte - self.top_byte;
                let bpr = bytes_per_row as u64;
                let row = (rel / bpr) as usize;
                if row >= self.visible_rows {
                    return None;
                }
                let inner = (rel % bpr) as usize;
                // "00000000  " = 10 列，每字节 "XX " = 3 列
                let col = 10 + inner * 3;
                Some((row, col.min(self.visible_cols.saturating_sub(1))))
            }
            OpenMode::Text | OpenMode::Auto => {
                let cursor_line = self.line_index.offset_to_line(editor, editor.cursor_byte).await.0;
                let top_line = self.top_line(editor).await;
                if cursor_line < top_line {
                    return None;
                }
                let row = (cursor_line - top_line) as usize;
                if row >= self.visible_rows {
                    return None;
                }
                let col = self.line_index.offset_to_line(editor, editor.cursor_byte).await.1;
                Some((row, (col as usize).min(self.visible_cols.saturating_sub(1))))
            }
        }
    }

    /// 确保光标可见：如果光标在视口外，滚动到可见。
    /// 文本模式按真实逻辑行；Hex 模式按 bytes_per_line。
    pub async fn ensure_cursor_visible(&mut self, editor: &Editor) {
        match editor.mode {
            OpenMode::Binary => {
                let per_line = self.bytes_per_line(editor);
                let cursor_line = editor.cursor_byte / per_line;
                let top_line = self.top_byte / per_line;
                let bottom_line = top_line + self.visible_rows as u64;

                if cursor_line < top_line {
                    self.top_byte = cursor_line * per_line;
                } else if cursor_line >= bottom_line {
                    let new_top = cursor_line.saturating_sub(self.visible_rows as u64 / 2);
                    self.top_byte = new_top * per_line;
                }
            }
            _ => {
                let cursor_line = self.line_index.offset_to_line(editor, editor.cursor_byte).await.0;
                let top_line = self.top_line(editor).await;
                if cursor_line < top_line {
                    self.top_byte = self.line_index.line_start(editor, cursor_line).await;
                } else if cursor_line >= top_line + self.visible_rows as u64 {
                    let new_top = cursor_line.saturating_sub(self.visible_rows as u64 / 2);
                    self.top_byte = self.line_index.line_start(editor, new_top).await;
                }
            }
        }
    }

    /// 从 `offset`（行首）读取一整行（不含换行符），返回 (内容, 下一行起始偏移)。
    /// 逐块读取，遇到换行符或文件末尾结束。
    async fn read_line(&self, editor: &Editor, offset: u64) -> (Vec<u8>, u64) {
        let mut out = Vec::new();
        let mut pos = offset;
        loop {
            if pos >= editor.file_size {
                return (out, pos);
            }
            let (block_id, _) = editor.delta_table.locate_offset(pos);
            let snap = match editor.cache.try_get(block_id) {
                Some(s) => s,
                None => match editor.ensure_block_loaded(block_id).await {
                    Ok(s) => s,
                    Err(_) => return (out, editor.file_size),
                },
            };
            let block_start = editor
                .delta_table
                .block_start_offset(block_id, editor.config.block_size);
            let inner = (pos - block_start) as usize;
            let data = &snap.contiguous[inner.min(snap.contiguous.len())..];
            match data.iter().position(|b| *b == b'\n') {
                Some(p) => {
                    out.extend_from_slice(&data[..p]);
                    return (out, pos + p as u64 + 1);
                }
                None => {
                    out.extend_from_slice(data);
                    pos += data.len() as u64;
                }
            }
        }
    }

    /// 批量获取文本模式整个视口的行内容（长度 = visible_rows）。
    ///
    /// 每一行是一个【真实逻辑行】（到下一个换行符为止）。返回项含义：
    /// - None：该行超出文件末尾
    /// - Some((offset, bytes))：行起始偏移 + 行内容（不含换行符）
    pub async fn text_rows(&self, editor: &Editor) -> Vec<Option<(u64, Vec<u8>)>> {
        let mut out = Vec::with_capacity(self.visible_rows);
        let mut offset = self.top_byte;
        for _ in 0..self.visible_rows {
            if offset >= editor.file_size {
                out.push(None);
                continue;
            }
            let (bytes, next) = self.read_line(editor, offset).await;
            out.push(Some((offset, bytes)));
            offset = next;
        }
        out
    }

    /// 批量获取 Hex 模式整个视口的行字节范围（长度 = visible_rows）。
    ///
    /// bytes_per_row 由调用方传入（窄终端可能降级为 8），保证行切分与渲染一致。
    /// 每个块只取一次快照，避免逐行 try_get 整块克隆 N 次。
    pub fn hex_rows(
        &self,
        editor: &Editor,
        bytes_per_row: usize,
    ) -> Vec<Option<(u64, Vec<u8>)>> {
        let bpr = bytes_per_row as u64;
        let mut block_snaps: HashMap<u64, BlockSnapshot> = HashMap::new();
        let mut out = Vec::with_capacity(self.visible_rows);
        for row in 0..self.visible_rows {
            let start = self.top_byte + (row as u64 * bpr);
            if start >= editor.file_size {
                out.push(None);
                continue;
            }
            let end = (start + bpr).min(editor.file_size);
            let block_id = start / editor.config.block_size as u64;
            let block_start = block_id * editor.config.block_size as u64;
            let inner_start = (start - block_start) as usize;
            let inner_end = (end - block_start) as usize;

            let snap = match block_snaps.entry(block_id) {
                Entry::Occupied(s) => s.into_mut(),
                Entry::Vacant(v) => match editor.cache.try_get(block_id) {
                    Some(s) => v.insert(s),
                    None => {
                        out.push(Some((start, vec![])));
                        continue;
                    }
                },
            };
            let end_idx = inner_end.min(snap.contiguous.len());
            let start_idx = inner_start.min(end_idx);
            out.push(Some((start, snap.contiguous[start_idx..end_idx].to_vec())));
        }
        out
    }
}
