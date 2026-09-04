//! 惰性行索引：按需扫描块的换行符位置，把"行号 ↔ 字节偏移"映射缓存起来。
//!
//! 文件可能达 GB 级，因此不一次性加载全部行号；只缓存已扫描块的换行符位置
//! 和少量已知"行号 → 字节偏移"，靠导航局部性保持查询廉价。
//! 块内容通过 `Editor::ensure_block_loaded` 读取（缓存 → WAL → 基础文件），
//! 因此反映当前编辑状态。
//!
//! 编辑会使缓存失效：Editor 维护 `edit_epoch`，每次编辑自增；LineIndex 记录
//! 上次生效的 epoch，发现过期就整体清空（编辑相对渲染低频，整体清空足够）。

use std::collections::{BTreeMap, HashMap};
use crate::block::BlockId;
use crate::editor::Editor;

/// 行号 → 字节偏移（已确定的映射）
pub struct LineIndex {
    /// 每个逻辑块内换行符相对块起始的偏移（已扫描的块）
    block_newlines: HashMap<BlockId, Vec<u32>>,
    /// 已知的行号 → 字节偏移（有序，便于范围查找最近的已知行）
    line_starts: BTreeMap<u64, u64>,
    /// 文件总行数缓存
    total_lines_cache: Option<u64>,
    /// 上次生效的编辑纪元
    epoch: u64,
}

impl LineIndex {
    pub fn new() -> Self {
        let mut line_starts = BTreeMap::new();
        line_starts.insert(0, 0); // 第 0 行从文件头开始
        Self {
            block_newlines: HashMap::new(),
            line_starts,
            total_lines_cache: None,
            epoch: 0,
        }
    }

    /// 若编辑纪元变化，整体清空缓存。
    fn ensure_fresh(&mut self, editor: &Editor) {
        if self.epoch != editor.edit_epoch {
            self.invalidate();
            self.epoch = editor.edit_epoch;
        }
    }

    /// 清空所有缓存（编辑后调用）
    pub fn invalidate(&mut self) {
        self.block_newlines.clear();
        self.line_starts.clear();
        self.line_starts.insert(0, 0);
        self.total_lines_cache = None;
    }

    /// 扫描并缓存某块的换行符位置（相对块起始）
    async fn block_newlines(&mut self, editor: &Editor, block_id: BlockId) -> Vec<u32> {
        self.ensure_fresh(editor);
        if let Some(v) = self.block_newlines.get(&block_id) {
            return v.clone();
        }
        let snap = match editor.ensure_block_loaded(block_id).await {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let v = snap
            .contiguous
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i as u32)
            .collect::<Vec<_>>();
        self.block_newlines.insert(block_id, v.clone());
        v
    }

    /// 从 `from`（行首）向前数 `n` 个换行符，返回第 n 个换行符之后的位置
    /// （不足 n 个则返回文件末尾）。
    async fn advance_lines(&mut self, editor: &Editor, from: u64, n: u64) -> u64 {
        if n == 0 {
            return from;
        }
        let mut remaining = n;
        let mut offset = from;
        while remaining > 0 && offset < editor.file_size {
            let (block_id, _) = editor.delta_table.locate_offset(offset);
            let nls = self.block_newlines(editor, block_id).await;
            let block_start = editor
                .delta_table
                .block_start_offset(block_id, editor.config.block_size);
            let inner = (offset - block_start) as usize;
            // 该块内位于 offset 之后（含 offset）的换行符
            let after: Vec<u32> = nls.iter().copied().filter(|&p| (p as usize) >= inner).collect();
            if (after.len() as u64) >= remaining {
                let p = after[(remaining - 1) as usize];
                return block_start + p as u64 + 1;
            }
            remaining -= after.len() as u64;
            // 跳到下一块；该块末尾可能没有换行符，行继续到下一块
            let next = block_start + editor.delta_table.block_size(block_id) as u64;
            if next >= editor.file_size {
                return editor.file_size;
            }
            offset = next;
        }
        editor.file_size
    }

    /// 第 `line` 行的字节起始偏移（越界则夹到文件末尾）。
    pub async fn line_start(&mut self, editor: &Editor, line: u64) -> u64 {
        self.ensure_fresh(editor);
        if let Some(&b) = self.line_starts.get(&line) {
            return b;
        }
        // 从最近的已知行起点向前推进
        let (cur_line, cur_offset) = match self.line_starts.range(..=line).next_back() {
            Some((&l, &b)) => (l, b),
            None => (0, 0),
        };
        let offset = self.advance_lines(editor, cur_offset, line - cur_line).await;
        self.line_starts.insert(line, offset);
        offset
    }

    /// 文件总行数（空文件 = 1 行）。
    pub async fn total_lines(&mut self, editor: &Editor) -> u64 {
        self.ensure_fresh(editor);
        if let Some(t) = self.total_lines_cache {
            return t;
        }
        let mut count = 0u64;
        for block in 0..editor.block_count {
            count += self.block_newlines(editor, block).await.len() as u64;
        }
        let total = count + 1;
        self.total_lines_cache = Some(total);
        total
    }

    /// 字节偏移所在的行号与行内字节列（第 0 列开始）。
    pub async fn offset_to_line(&mut self, editor: &Editor, offset: u64) -> (u64, u64) {
        self.ensure_fresh(editor);
        let offset = offset.min(editor.file_size);
        // 找最近一个【字节偏移 ≤ offset】的已知行起点（line_starts 键是行号，
        // 必须按字节偏移值筛选，不能按行号查）
        let (mut line, cur_offset) = {
            let mut best: Option<(u64, u64)> = None;
            for (&l, &b) in &self.line_starts {
                if b <= offset && best.map_or(true, |(_, bb)| b > bb) {
                    best = Some((l, b));
                }
            }
            best.unwrap_or((0, 0))
        };
        // 从 cur_offset 走到 offset，数换行符并记录最后一个换行符位置
        let mut last_nl: Option<u64> = None;
        let mut pos = cur_offset;
        while pos < offset {
            let (block_id, _) = editor.delta_table.locate_offset(pos);
            let nls = self.block_newlines(editor, block_id).await;
            let block_start = editor
                .delta_table
                .block_start_offset(block_id, editor.config.block_size);
            let inner = (pos - block_start) as usize;
            let limit = (offset - block_start) as usize; // 只统计 offset 之前的换行符
            for &p in &nls {
                let p = p as usize;
                if p >= inner && p < limit {
                    line += 1;
                    last_nl = Some(block_start + p as u64);
                }
            }
            let block_end = block_start + editor.delta_table.block_size(block_id) as u64;
            if block_end >= offset {
                break;
            }
            pos = block_end;
        }
        let col = offset - last_nl.map(|p| p + 1).unwrap_or(cur_offset);
        // 缓存此行起点，便于后续导航
        let line_start = offset - col;
        self.line_starts.entry(line).or_insert(line_start);
        (line, col)
    }
}

impl Default for LineIndex {
    fn default() -> Self {
        Self::new()
    }
}
