use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use encoding_rs::Encoding;
use crate::block::{Block, BlockId};
use crate::cache::{BlockCache, BlockSnapshot};
use crate::config::{EditorConfig, OpenMode};
use crate::delta_table::DeltaTable;
use crate::error::{EditError, IoError};
use crate::io_worker::compute_block_count;
use crate::message::{Direction, IoRequest, SaveReport, SearchResult};

#[derive(Debug)]
pub enum UndoEntry {
    InsertBytes {
        block_id: BlockId,
        inner_pos: usize,
        bytes: Vec<u8>,
        cursor_before: u64,
        /// 若此次插入触发了块分裂，记录 (新块 id, 分裂点)。
        /// undo 时先合并回分裂前的块布局（使 inner_pos 有效）再删字节。
        split: Option<(BlockId, usize)>,
    },
    DeleteBytes { block_id: BlockId, inner_pos: usize, bytes: Vec<u8>, cursor_before: u64 },
}

pub struct Editor {
    pub config: Arc<EditorConfig>,
    pub cache: Arc<BlockCache>,
    pub io_tx: mpsc::Sender<IoRequest>,
    pub file_path: PathBuf,
    pub file_size: u64,
    pub original_file_size: u64,
    pub block_count: u64,
    pub cursor_byte: u64,
    pub mode: OpenMode,
    pub text_encoding: &'static Encoding,
    pub undo_stack: Vec<UndoEntry>,
    pub redo_stack: Vec<UndoEntry>,
    pub delta_table: DeltaTable,
    pub should_quit: bool,
    /// 最近一次非空搜索的字节模式，供 n/N 重复搜索
    pub last_search: Option<Vec<u8>>,
    /// 编辑纪元：每次编辑自增，LineIndex 据此判断缓存是否过期
    pub edit_epoch: u64,
    /// 只读模式：阻止插入/删除/撤销/重做/保存（默认由 main 按 CLI 设为 true）
    pub read_only: bool,
}

impl Editor {
    pub async fn open(
        file_path: PathBuf,
        config: Arc<EditorConfig>,
        mode: OpenMode,
        text_encoding: &'static Encoding,
        io_tx: mpsc::Sender<IoRequest>,
        cache: Arc<BlockCache>,
    ) -> Result<Self, IoError> {
        let meta = std::fs::metadata(&file_path)?;
        let file_size = meta.len();
        let block_size = config.block_size;
        let block_count = compute_block_count(file_size, block_size);
        let delta_table = DeltaTable::new(block_count, block_size, file_size);
        Ok(Self {
            config,
            cache,
            io_tx,
            file_path,
            file_size,
            original_file_size: file_size,
            block_count,
            cursor_byte: 0,
            mode,
            text_encoding,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            delta_table,
            should_quit: false,
            last_search: None,
            edit_epoch: 0,
            read_only: false,
        })
    }

    pub fn goto_byte(&mut self, byte: u64) {
        self.cursor_byte = byte.min(self.file_size);
    }

    /// 按行移动光标（保持列位置）。
    ///
    /// - Binary：按 bytes_per_line（字节/行）移动，列保持为字节偏移
    /// - Text/Auto：按【真实逻辑行】移动（经 LineIndex），列保持为字节列
    pub async fn move_cursor_lines(
        &mut self,
        lines: i64,
        bytes_per_line: u64,
        line_index: &mut crate::line_index::LineIndex,
    ) {
        match self.mode {
            OpenMode::Binary => {
                let delta = lines * bytes_per_line as i64;
                let new = (self.cursor_byte as i64 + delta).clamp(0, self.file_size as i64) as u64;
                self.cursor_byte = new;
            }
            _ => {
                let total = line_index.total_lines(self).await;
                if total <= 1 {
                    self.cursor_byte = 0;
                    return;
                }
                let (cur_line, _) = line_index.offset_to_line(self, self.cursor_byte).await;
                let new_line = ((cur_line as i64 + lines).clamp(0, total as i64 - 1)) as u64;
                let new_start = line_index.line_start(self, new_line).await;
                // 保持字节列，并夹到目标行长度
                let cur_start = line_index.line_start(self, cur_line).await;
                let col = self.cursor_byte - cur_start;
                let line_len = if new_line + 1 < total {
                    line_index.line_start(self, new_line + 1).await - new_start
                } else {
                    self.file_size - new_start
                };
                self.cursor_byte = new_start + col.min(line_len);
            }
        }
    }

    /// 确保某个块已加载并返回其 Snapshot
    pub async fn ensure_block_loaded(&self, block_id: BlockId) -> Result<BlockSnapshot, IoError> {
        if let Some(s) = self.cache.try_get(block_id) {
            return Ok(s);
        }
        let (tx, rx) = oneshot::channel();
        self.io_tx
            .send(IoRequest::LoadBlock { block_id, reply: tx })
            .await
            .map_err(|e| IoError::SaveFailed(format!("send LoadBlock: {e}")))?;
        let bytes = rx
            .await
            .map_err(|e| IoError::SaveFailed(format!("LoadBlock reply canceled: {e}")))??;
        let block = Block::new_clean(block_id, bytes);
        self.cache.insert_loaded(block);
        self.cache
            .try_get(block_id)
            .ok_or_else(|| IoError::SaveFailed("cache insert 后仍 miss".into()))
    }

    pub async fn insert_bytes_at_cursor(&mut self, bytes: &[u8]) -> Result<(), EditError> {
        if self.read_only {
            return Err(EditError::GapBuffer("只读模式，禁止修改".into()));
        }
        // 首次编辑惰性创建 WAL（脏块淘汰/增量保存需要）
        self.ensure_wal()
            .await
            .map_err(|e| EditError::GapBuffer(format!("WAL 初始化失败: {e}")))?;
        if self.cursor_byte > self.file_size {
            return Err(EditError::OffsetOutOfRange(self.cursor_byte, self.file_size));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        // 空文件特殊处理：需要创建块 0
        if self.file_size == 0 && self.block_count == 0 {
            let b = Block::new_clean(0, vec![]);
            self.cache.insert_loaded(b);
            self.block_count = 1;
            self.delta_table = DeltaTable::new(1, self.config.block_size, 0);
        }
        let (block_id, inner_pos) = self.delta_table.locate_offset(self.cursor_byte);
        // 确保块已加载
        self.ensure_block_loaded(block_id)
            .await
            .map_err(|_| EditError::GapBuffer(format!("加载块 {block_id} 失败")))?;
        // pin + insert
        let _guard = self
            .cache
            .pin_for_edit(block_id)
            .map_err(|_| EditError::GapBuffer(format!("pin 块 {block_id} 失败")))?;
        self.cache.with_pin_mut(block_id, |gb| {
            gb.insert(inner_pos, bytes)?;
            Ok::<_, EditError>(())
        })?;
        drop(_guard);
        // 更新 delta_table
        let current_delta = self.delta_table.block_delta(block_id);
        let new_delta = current_delta + bytes.len() as i64;
        self.delta_table.record_delta(block_id, new_delta);
        // 更新 file_size
        self.file_size += bytes.len() as u64;
        // undo：记录插入前光标位置；分裂与插入属同一动作，分裂信息并入该条目，
        // 一次 undo 即可完整还原（先合并分裂，再删字节）
        let cursor_before = self.cursor_byte;
        self.cursor_byte += bytes.len() as u64;
        let split = self.maybe_split_block(block_id);
        self.undo_stack.push(UndoEntry::InsertBytes {
            block_id,
            inner_pos,
            bytes: bytes.to_vec(),
            cursor_before,
            split,
        });
        self.redo_stack.clear();
        if self.undo_stack.len() > self.config.undo_limit {
            let drain = self.undo_stack.len() - self.config.undo_limit;
            self.undo_stack.drain(..drain);
        }
        self.edit_epoch += 1;
        Ok(())
    }

    pub async fn delete_bytes_at_cursor_forward(&mut self, len: usize) -> Result<(), EditError> {
        if self.read_only {
            return Err(EditError::GapBuffer("只读模式，禁止修改".into()));
        }
        self.ensure_wal()
            .await
            .map_err(|e| EditError::GapBuffer(format!("WAL 初始化失败: {e}")))?;
        if len == 0 {
            return Ok(());
        }
        if self.cursor_byte + len as u64 > self.file_size {
            return Err(EditError::OffsetOutOfRange(
                self.cursor_byte + len as u64,
                self.file_size,
            ));
        }
        let (block_id, inner_pos) = self.delta_table.locate_offset(self.cursor_byte);
        self.ensure_block_loaded(block_id)
            .await
            .map_err(|_| EditError::GapBuffer(format!("加载块 {block_id} 失败")))?;
        // 读取要删除的字节（供 undo）
        let snap = self
            .cache
            .try_get(block_id)
            .ok_or_else(|| EditError::GapBuffer("块 miss".into()))?;
        let deleted_bytes = snap
            .contiguous
            .get(inner_pos..inner_pos + len)
            .ok_or_else(|| EditError::GapBuffer("跨块删除在 Task9 实现".into()))?
            .to_vec();
        let _guard = self
            .cache
            .pin_for_edit(block_id)
            .map_err(|_| EditError::GapBuffer(format!("pin 块 {block_id} 失败")))?;
        self.cache
            .with_pin_mut(block_id, |gb| gb.delete(inner_pos, len))?;
        drop(_guard);
        // 更新 delta
        let current_delta = self.delta_table.block_delta(block_id);
        let new_delta = current_delta - len as i64;
        self.delta_table.record_delta(block_id, new_delta);
        self.file_size -= len as u64;
        self.undo_stack.push(UndoEntry::DeleteBytes {
            block_id,
            inner_pos,
            bytes: deleted_bytes,
            cursor_before: self.cursor_byte,
        });
        self.redo_stack.clear();
        if self.undo_stack.len() > self.config.undo_limit {
            let drain = self.undo_stack.len() - self.config.undo_limit;
            self.undo_stack.drain(..drain);
        }
        self.edit_epoch += 1;
        Ok(())
    }

    pub async fn undo(&mut self) -> Result<(), EditError> {
        if self.read_only {
            return Err(EditError::GapBuffer("只读模式，禁止修改".into()));
        }
        self.ensure_wal()
            .await
            .map_err(|e| EditError::GapBuffer(format!("WAL 初始化失败: {e}")))?;
        let entry = self.undo_stack.pop().ok_or(EditError::NothingToUndo)?;
        match entry {
            UndoEntry::InsertBytes { block_id, inner_pos, bytes, cursor_before, split } => {
                // 若插入触发了分裂，先合并回分裂前的块布局（使 inner_pos 有效）
                if let Some((new_block_id, _)) = split {
                    self.cache.merge_blocks(block_id, new_block_id)
                        .map_err(|e| EditError::GapBuffer(format!("undo 分裂合并失败: {e}")))?;
                    self.delta_table.merge_block(block_id, new_block_id);
                    self.block_count -= 1;
                }
                let _ = self.ensure_block_loaded(block_id).await;
                let _guard = self.cache.pin_for_edit(block_id)
                    .map_err(|_| EditError::GapBuffer(format!("pin 块 {block_id} 失败")))?;
                self.cache.with_pin_mut(block_id, |gb| gb.delete(inner_pos, bytes.len()))?;
                drop(_guard);
                let cur = self.delta_table.block_delta(block_id);
                self.delta_table.record_delta(block_id, cur - bytes.len() as i64);
                self.file_size -= bytes.len() as u64;
                // 光标恢复到插入前位置
                self.cursor_byte = cursor_before.min(self.file_size);
                self.redo_stack.push(UndoEntry::InsertBytes { block_id, inner_pos, bytes, cursor_before, split });
            }
            UndoEntry::DeleteBytes { block_id, inner_pos, bytes, cursor_before } => {
                let _ = self.ensure_block_loaded(block_id).await;
                let _guard = self.cache.pin_for_edit(block_id)
                    .map_err(|_| EditError::GapBuffer(format!("pin 块 {block_id} 失败")))?;
                self.cache.with_pin_mut(block_id, |gb| gb.insert(inner_pos, &bytes))?;
                drop(_guard);
                let cur = self.delta_table.block_delta(block_id);
                self.delta_table.record_delta(block_id, cur + bytes.len() as i64);
                self.file_size += bytes.len() as u64;
                // 光标恢复到删除前位置
                self.cursor_byte = cursor_before.min(self.file_size);
                self.redo_stack.push(UndoEntry::DeleteBytes { block_id, inner_pos, bytes, cursor_before });
            }
        }
        self.edit_epoch += 1;
        Ok(())
    }

    pub async fn redo(&mut self) -> Result<(), EditError> {
        if self.read_only {
            return Err(EditError::GapBuffer("只读模式，禁止修改".into()));
        }
        self.ensure_wal()
            .await
            .map_err(|e| EditError::GapBuffer(format!("WAL 初始化失败: {e}")))?;
        let entry = self.redo_stack.pop().ok_or(EditError::NothingToRedo)?;
        match entry {
            UndoEntry::InsertBytes { block_id, inner_pos, bytes, cursor_before, split } => {
                let _ = self.ensure_block_loaded(block_id).await;
                let _guard = self.cache.pin_for_edit(block_id)
                    .map_err(|_| EditError::GapBuffer(format!("pin 块 {block_id} 失败")))?;
                self.cache.with_pin_mut(block_id, |gb| gb.insert(inner_pos, &bytes))?;
                drop(_guard);
                let cur = self.delta_table.block_delta(block_id);
                self.delta_table.record_delta(block_id, cur + bytes.len() as i64);
                self.file_size += bytes.len() as u64;
                // redo 后光标到插入内容末尾
                self.cursor_byte = (cursor_before + bytes.len() as u64).min(self.file_size);
                // 若原来分裂过，重新分裂以还原块布局
                if let Some((new_block_id, split_at)) = split {
                    let right_len = self.cache.split_block(block_id, split_at)
                        .map_err(|e| EditError::GapBuffer(format!("redo 分裂失败: {e}")))?;
                    self.delta_table.insert_block_after(block_id, 0);
                    self.delta_table.record_delta(new_block_id, right_len as i64);
                    self.block_count += 1;
                }
                self.undo_stack.push(UndoEntry::InsertBytes { block_id, inner_pos, bytes, cursor_before, split });
            }
            UndoEntry::DeleteBytes { block_id, inner_pos, bytes, cursor_before } => {
                let _ = self.ensure_block_loaded(block_id).await;
                let _guard = self.cache.pin_for_edit(block_id)
                    .map_err(|_| EditError::GapBuffer(format!("pin 块 {block_id} 失败")))?;
                self.cache.with_pin_mut(block_id, |gb| gb.delete(inner_pos, bytes.len()))?;
                drop(_guard);
                let cur = self.delta_table.block_delta(block_id);
                self.delta_table.record_delta(block_id, cur - bytes.len() as i64);
                self.file_size -= bytes.len() as u64;
                // redo 后光标保持在删除起点
                self.cursor_byte = cursor_before.min(self.file_size);
                self.undo_stack.push(UndoEntry::DeleteBytes { block_id, inner_pos, bytes, cursor_before });
            }
        }
        self.edit_epoch += 1;
        Ok(())
    }

    /// 若块过大则分裂（仅最后一块）。返回 Some((新块 id, 分裂点)) 表示发生了分裂。
    fn maybe_split_block(&mut self, block_id: BlockId) -> Option<(BlockId, usize)> {
        let threshold = (self.config.block_size as f64 * 1.5) as usize;
        let snap = self.cache.try_get(block_id)?;
        if snap.len <= threshold {
            return None;
        }
        // 仅在最后一块时分裂，避免 block_id 碰撞
        if block_id + 1 < self.block_count {
            return None;
        }
        let mid = snap.len / 2;
        let _guard = match self.cache.pin_for_edit(block_id) {
            Ok(g) => g,
            Err(_) => return None,
        };
        let right_bytes = match self.cache.with_pin_mut(block_id, |gb| {
            let right = gb.split_at(mid)?;
            Ok::<_, EditError>(right.as_contiguous())
        }) {
            Ok(b) => b,
            Err(_) => { return None; }
        };
        drop(_guard);
        let new_block_id = block_id + 1;
        let new_block = Block::new_clean(new_block_id, right_bytes);
        let right_len = new_block.len() as i64;
        self.cache.insert_loaded(new_block);
        self.delta_table.insert_block_after(block_id, 0);
        // 修正 delta_table 中分裂后两块的真实大小：
        // - 原块只保留前半（mid 字节）
        // - 新块包含后半（right_len 字节，初始为 0，需补 delta）
        let cur_size = self.delta_table.block_size(block_id);
        let orig_size = cur_size - self.delta_table.block_delta(block_id);
        let left_len = mid as i64;
        self.delta_table.record_delta(block_id, left_len - orig_size);
        self.delta_table.record_delta(new_block_id, right_len);
        self.block_count += 1;
        Some((new_block_id, mid))
    }

    /// 流式保存/折叠：把【逻辑文件】（缓存 → WAL → 基础文件）逐块写入目标，原子 rename。
    ///
    /// ⚠️ 不变量（R1）：基础文件在会话期间**只读**且保持块对齐（block i 位于 i×block_size），
    /// 编辑只写 WAL。本函数是唯一改写基础文件的地方：
    /// - 折叠到【自身路径】会破坏块对齐（写入的是逻辑块的拼接），因此只能在**退出时**
    ///   调用（:wq / 退出合并），完成后立即删除 WAL 临时文件（内容已并入基础文件），
    ///   之后进程结束、不再有按物理偏移的读取；下一进程会从新文件重新建立块模型。
    /// - 折叠到【其他路径】不动原文件，WAL 保留，编辑仍与原始文件关联。
    ///
    /// 若未来要在会话中途折叠到自身，必须先重建 delta_table/block_count 等块模型，
    /// 否则后续按 i×block_size 读基础文件会得到错位数据。
    pub async fn save_as(&mut self, target: PathBuf) -> Result<(), IoError> {
        use tokio::io::AsyncWriteExt;

        let fold_into_self = target == self.file_path;
        let tmp_path = target.with_extension("beditor-tmp");
        let mut out = tokio::fs::File::create(&tmp_path).await?;
        let mut written_bytes: u64 = 0;
        let block_size = self.config.block_size as u64;
        let disk_file_size = std::fs::metadata(&self.file_path)
            .map(|m| m.len())
            .unwrap_or(0);
        // 原文件句柄只打开一次，供基础块读取复用（避免每块重开文件）
        let mut disk = tokio::fs::File::open(&self.file_path).await?;

        for block_id in 0..self.block_count {
            // 1) 缓存（标记 clean 并取走）
            if self.cache.try_get(block_id).is_some() {
                if let Some(bytes) = self.cache.take_contiguous_and_mark_clean(block_id) {
                    out.write_all(&bytes).await?;
                    written_bytes += bytes.len() as u64;
                    continue;
                }
            }
            // 2) 其余（WAL → 基础文件）走全项目共享的逻辑块读取
            let bytes = crate::io_worker::logical_block_bytes(
                &self.cache,
                self.cache.wal().as_deref(),
                Some(&mut disk),
                &self.file_path,
                block_id,
                block_size as usize,
                disk_file_size,
            )
            .await?;
            out.write_all(&bytes).await?;
            written_bytes += bytes.len() as u64;
        }

        out.flush().await?;
        out.sync_all().await?;
        drop(out);

        tokio::fs::rename(&tmp_path, &target)
            .await
            .map_err(|e| IoError::SaveFailed(format!("rename 失败: {e}")))?;

        self.file_size = written_bytes;
        self.original_file_size = written_bytes;
        // 折叠到自身会破坏基础文件块对齐，必须清理 WAL（R1 不变量）。
        // 内容已全部写入基础文件，临时文件一并删除，不再残留零字节文件。
        if fold_into_self {
            self.dispose_wal(true).await?;
            debug_assert!(!self.wal_pending(), "折叠到自身后 WAL 必须清空");
        }
        Ok(())
    }

    /// 增量保存（:w）：把所有脏块写入 WAL 并 fsync，不改动基础文件。
    /// 大文件秒存；WAL 会在退出/下次合并时写入基础文件。
    pub async fn save_incremental(&mut self) -> Result<SaveReport, IoError> {
        let (tx, rx) = oneshot::channel();
        self.io_tx
            .send(IoRequest::CommitSave { reply: tx })
            .await
            .map_err(|e| IoError::SaveFailed(format!("send CommitSave: {e}")))?;
        let report = rx
            .await
            .map_err(|e| IoError::SaveFailed(format!("CommitSave reply: {e}")))?;
        // 脏块已写入 WAL，磁盘上不再是"未保存"状态
        report
    }

    /// 是否存在未合并的 WAL 增量（打开时发现 / 退出前需合并）
    pub fn wal_pending(&self) -> bool {
        self.cache.wal().map(|w| w.has_entries()).unwrap_or(false)
    }

    /// 确保 WAL 已启用（惰性创建 `<file>.beditor-wal` 并挂接）。
    /// 幂等：已启用则直接返回。只读模式不调用（不产生临时文件）。
    pub async fn ensure_wal(&self) -> Result<(), IoError> {
        if self.cache.wal().is_some() {
            return Ok(());
        }
        let (tx, rx) = oneshot::channel();
        self.io_tx
            .send(IoRequest::EnableWal { reply: tx })
            .await
            .map_err(|e| IoError::SaveFailed(format!("send EnableWal: {e}")))?;
        rx.await
            .map_err(|e| IoError::SaveFailed(format!("EnableWal reply canceled: {e}")))?
    }

    /// 摘除 WAL；`delete = true` 时删除临时文件。
    /// 完整合并写入基础文件（save_as 折叠到自身）后调用，清理残留。
    pub async fn dispose_wal(&self, delete: bool) -> Result<(), IoError> {
        let (tx, rx) = oneshot::channel();
        self.io_tx
            .send(IoRequest::DisposeWal { delete, reply: tx })
            .await
            .map_err(|e| IoError::SaveFailed(format!("send DisposeWal: {e}")))?;
        rx.await
            .map_err(|e| IoError::SaveFailed(format!("DisposeWal reply canceled: {e}")))?
    }

    pub async fn search_literal(
        &mut self,
        query: &[u8],
        direction: Direction,
        limit: usize,
    ) -> Result<SearchResult, IoError> {
        // 记录非空查询，供 n/N 重复搜索
        if !query.is_empty() {
            self.last_search = Some(query.to_vec());
        }
        // 定位起始块及其逻辑偏移（基于编辑后的 delta_table，而非磁盘布局）
        let start_block = self.delta_table.locate_offset(self.cursor_byte).0;
        let start_block_offset = self.delta_table.block_start_offset(start_block, self.config.block_size);
        let (tx, rx) = oneshot::channel();
        self.io_tx
            .send(IoRequest::SearchLiteral {
                query: query.to_vec(),
                start_byte: self.cursor_byte,
                start_block,
                start_block_offset,
                logical_block_count: self.block_count,
                direction,
                limit,
                reply: tx,
            })
            .await
            .map_err(|e| IoError::SaveFailed(format!("send search: {e}")))?;
        rx.await
            .map_err(|e| IoError::SaveFailed(format!("search reply: {e}")))
    }
}
