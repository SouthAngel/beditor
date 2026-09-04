use std::ops::Range;
use tokio::sync::oneshot;
use crate::error::IoError;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Direction { Forward, Backward }

#[derive(Debug)]
pub struct SaveReport {
    pub written_bytes: u64,
    pub dirty_blocks_written: usize,
    pub clean_blocks_copied: usize,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct SearchResult {
    pub matches: Vec<u64>,
    pub scanned_blocks: usize,
    pub has_more_behind: bool,
}

#[derive(Debug)]
pub enum IoRequest {
    LoadBlock {
        block_id: u64,
        reply: oneshot::Sender<Result<Vec<u8>, IoError>>,
    },
    PrefetchRange {
        start_block: u64,
        end_block: u64,
    },
    FlushBlock {
        block_id: u64,
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), IoError>>,
    },
    /// 增量保存：把所有脏块写入 WAL 并 fsync（不改动基础文件）
    CommitSave {
        reply: oneshot::Sender<Result<SaveReport, IoError>>,
    },
    SearchLiteral {
        query: Vec<u8>,
        start_byte: u64,
        /// 起始逻辑块 id（由 Editor 经 delta_table 定位，含编辑后布局）
        start_block: u64,
        /// 起始块的逻辑字节偏移（delta_table.block_start_offset）
        start_block_offset: u64,
        /// 逻辑块总数（编辑后，可能因分裂 > 磁盘块数）
        logical_block_count: u64,
        direction: Direction,
        limit: usize,
        reply: oneshot::Sender<SearchResult>,
    },
    ReadFileHeader {
        max_bytes: usize,
        reply: oneshot::Sender<Vec<u8>>,
    },
}

#[derive(Debug)]
pub enum IoEvent {
    PrefetchCompleted { range: Range<u64>, loaded: usize },
    BlockFlushed { block_id: u64 },
    SaveProgress { ratio: f32 },
    Error(IoError),
}
