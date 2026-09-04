use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use sysinfo::System;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;
use tracing::{debug, error, info};
use crate::block::BlockId;
use crate::cache::BlockCache;
use crate::config::EditorConfig;
use crate::error::IoError;
use crate::message::{Direction, IoEvent, IoRequest, SaveReport, SearchResult};
use crate::wal::Wal;

/// 计算一个文件的逻辑块数
pub fn compute_block_count(file_size: u64, block_size: usize) -> u64 {
    if file_size == 0 { 0 } else { (file_size - 1) / block_size as u64 + 1 }
}

/// 根据 sysinfo + config.mem_ratio 计算 LRU 容量（块数量）
pub fn compute_capacity_blocks(cfg: &EditorConfig) -> usize {
    let sys_total = {
        let mut s = System::new();
        s.refresh_memory();
        s.total_memory() as usize
    };
    let cap_bytes = (sys_total as f64 * cfg.mem_ratio as f64) as usize;
    (cap_bytes / cfg.block_size).max(4)
}

pub async fn spawn_io_workers(
    file_path: PathBuf,
    cfg: Arc<EditorConfig>,
    mut io_rx: mpsc::Receiver<IoRequest>,
    event_tx: mpsc::Sender<IoEvent>,
) -> Result<Arc<BlockCache>, IoError> {
    let capacity = compute_capacity_blocks(&cfg);
    info!(capacity, block_size = cfg.block_size, mem_ratio = %cfg.mem_ratio, "初始化 BlockCache");
    let cache = Arc::new(BlockCache::new(cfg.clone(), capacity));
    let cache_for_dispatch = cache.clone();
    let cfg_for_dispatch = cfg.clone();
    let file_path_for_dispatch = file_path.clone();

    // 打开主文件句柄（用于早期校验：文件可读）
    let _main_file = File::open(&file_path).await
        .map_err(|e| IoError::SaveFailed(format!("打开文件失败: {e}")))?;

    // 增量持久化：打开/回放 WAL 并挂到缓存（脏块淘汰时自动写入）
    let wal_path = file_path.with_extension("beditor-wal");
    let wal = Wal::open(wal_path)
        .map_err(|e| IoError::SaveFailed(format!("打开 WAL 失败: {e}")))?;
    if wal.has_entries() {
        info!(entries = wal.len(), "检测到未合并的 WAL 数据，已回放");
    }
    cache.attach_wal(wal.clone());
    let wal_for_dispatch = wal.clone();

    // dispatcher task
    tokio::spawn(async move {
        while let Some(req) = io_rx.recv().await {
            match req {
                IoRequest::LoadBlock { block_id, reply } => {
                    let fp = file_path_for_dispatch.clone();
                    let bs = cfg_for_dispatch.block_size;
                    let c = cache_for_dispatch.clone();
                    let w = wal_for_dispatch.clone();
                    tokio::spawn(async move {
                        let disk_file_size = std::fs::metadata(&fp).map(|m| m.len()).unwrap_or(0);
                        let result = logical_block_bytes(&c, Some(&w), None, &fp, block_id, bs, disk_file_size).await;
                        let _ = reply.send(result);
                    });
                }
                IoRequest::PrefetchRange { start_block, end_block } => {
                    let c = cache_for_dispatch.clone();
                    let fp = file_path_for_dispatch.clone();
                    let bs = cfg_for_dispatch.block_size;
                    let w = wal_for_dispatch.clone();
                    let ev = event_tx.clone();
                    tokio::spawn(async move {
                        let loaded = prefetch_impl(&fp, &c, &w, start_block, end_block, bs).await;
                        let _ = ev.send(IoEvent::PrefetchCompleted {
                            range: start_block..end_block,
                            loaded,
                        }).await;
                    });
                }
                IoRequest::ReadFileHeader { max_bytes, reply } => {
                    let fp = file_path_for_dispatch.clone();
                    tokio::spawn(async move {
                        let result = read_header_impl(&fp, max_bytes).await;
                        let _ = reply.send(result);
                    });
                }
                IoRequest::FlushBlock { block_id, data, reply } => {
                    let w = wal_for_dispatch.clone();
                    tokio::spawn(async move {
                        let result = w.append_one(block_id, &data)
                            .map_err(|e| IoError::SaveFailed(format!("WAL 追加失败: {e}")));
                        let _ = reply.send(result);
                    });
                }
                IoRequest::CommitSave { reply } => {
                    let c = cache_for_dispatch.clone();
                    let w = wal_for_dispatch.clone();
                    tokio::spawn(async move {
                        let result = commit_save_impl(&c, &w).await;
                        let _ = reply.send(result);
                    });
                }
                IoRequest::SearchLiteral { query, start_byte, start_block, start_block_offset, logical_block_count, direction, limit, reply } => {
                    let fp = file_path_for_dispatch.clone();
                    let bs = cfg_for_dispatch.block_size;
                    let c = cache_for_dispatch.clone();
                    let w = wal_for_dispatch.clone();
                    tokio::spawn(async move {
                        let result = search_impl(&fp, &c, &w, SearchArgs {
                            query,
                            start_byte,
                            start_block,
                            start_block_offset,
                            logical_block_count,
                            block_size: bs,
                            direction,
                            limit,
                        }).await;
                        let _ = reply.send(result);
                    });
                }
            }
        }
    });

    Ok(cache)
}

/// 从基础文件读取 block_id 对应的原始块（物理偏移 = id × block_size）。
async fn read_block_from_handle(
    disk: &mut tokio::fs::File,
    block_id: BlockId,
    disk_file_size: u64,
    block_size: usize,
) -> Result<Vec<u8>, IoError> {
    let off = block_id * block_size as u64;
    let expect = if off + block_size as u64 > disk_file_size {
        (disk_file_size - off) as usize
    } else {
        block_size
    };
    disk.seek(SeekFrom::Start(off)).await?;
    let mut buf = vec![0u8; expect];
    let mut total = 0usize;
    while total < expect {
        let n = disk.read(&mut buf[total..]).await?;
        if n == 0 { break; }
        total += n;
    }
    buf.truncate(total);
    Ok(buf)
}

async fn load_block_impl(
    file_path: &Path,
    block_id: BlockId,
    block_size: usize,
) -> Result<Vec<u8>, IoError> {
    let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
    let block_count = compute_block_count(file_size, block_size);
    if block_id >= block_count && file_size != 0 {
        return Err(IoError::BlockOutOfRange(block_id, block_count));
    }
    if file_size == 0 {
        return Ok(vec![]);
    }
    let mut f = File::open(file_path).await?;
    read_block_from_handle(&mut f, block_id, file_size, block_size).await
}

/// 读取逻辑块当前内容：缓存（最新编辑）→ WAL（已持久化增量）→ 基础文件（原始）。
///
/// 这是全项目唯一的"读一个逻辑块"实现，load/prefetch/search/折叠（save_as）
/// 均走这里，避免多处 cache→WAL→base 回退逻辑漂移。
///
/// `disk` 为可复用的基础文件句柄（save_as 折叠时传入以复用句柄）；None 时
/// 按需打开文件。
///
/// 不变量：基础文件在会话期间只读且保持块对齐（block i 位于 i×block_size），
/// 编辑只写 WAL。分裂新增的块（id ≥ 磁盘块数）只存在于缓存/WAL；
/// 磁盘上不存在时返回空 Vec（防御性，正常流程不会触发）。
pub async fn logical_block_bytes(
    cache: &BlockCache,
    wal: Option<&Wal>,
    disk: Option<&mut tokio::fs::File>,
    file_path: &Path,
    block_id: BlockId,
    block_size: usize,
    disk_file_size: u64,
) -> Result<Vec<u8>, IoError> {
    if let Some(s) = cache.try_get(block_id) {
        return Ok(s.contiguous);
    }
    if let Some(w) = wal {
        if let Some(bytes) = w
            .get(block_id)
            .map_err(|e| IoError::SaveFailed(format!("WAL 读取失败: {e}")))?
        {
            return Ok(bytes);
        }
    }
    let disk_block_count = compute_block_count(disk_file_size, block_size);
    if block_id >= disk_block_count {
        return Ok(Vec::new());
    }
    match disk {
        Some(f) => read_block_from_handle(f, block_id, disk_file_size, block_size).await,
        None => load_block_impl(file_path, block_id, block_size).await,
    }
}

/// CommitSave（:w 增量保存）：把所有脏块写入 WAL 并 fsync，不改动基础文件。
/// 返回写入报告；成功后这些块在缓存中标记为 Clean（内容已持久化）。
async fn commit_save_impl(cache: &BlockCache, wal: &Wal) -> Result<SaveReport, IoError> {
    let t0 = Instant::now();
    let dirty = cache.dirty_ids();
    let mut entries: Vec<(BlockId, Vec<u8>)> = Vec::new();
    let mut written: u64 = 0;
    for id in dirty {
        if let Some(bytes) = cache.take_contiguous_and_mark_clean(id) {
            written += bytes.len() as u64;
            entries.push((id, bytes));
        }
    }
    wal.append_many(&entries)
        .map_err(|e| IoError::SaveFailed(format!("WAL 追加失败: {e}")))?;
    Ok(SaveReport {
        written_bytes: written,
        dirty_blocks_written: entries.len(),
        clean_blocks_copied: 0,
        duration_ms: t0.elapsed().as_millis() as u64,
    })
}

async fn prefetch_impl(
    file_path: &Path,
    cache: &Arc<BlockCache>,
    wal: &Wal,
    start: BlockId,
    end: BlockId,
    block_size: usize,
) -> usize {
    let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
    let block_count = compute_block_count(file_size, block_size);
    let end_exclusive = end.min(block_count);
    let mut loaded = 0;
    for id in start..end_exclusive {
        if cache.try_get(id).is_some() { continue; }
        match logical_block_bytes(cache, Some(wal), None, file_path, id, block_size, file_size).await {
            Ok(bytes) => {
                let block = crate::block::Block::new_clean(id, bytes);
                cache.insert_loaded(block);
                loaded += 1;
            }
            Err(e) => {
                error!(block_id = id, err = %e, "prefetch 块失败");
            }
        }
    }
    debug!(start, end_exclusive, loaded, "prefetch 完成");
    loaded
}

async fn read_header_impl(file_path: &Path, max_bytes: usize) -> Vec<u8> {
    let mut f = match File::open(file_path).await {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let mut buf = vec![0u8; max_bytes];
    let n = f.read(&mut buf).await.unwrap_or(0);
    buf.truncate(n);
    buf
}

/// 一次搜索任务的参数（打包成结构体，避免 search_impl 参数过多）
struct SearchArgs {
    query: Vec<u8>,
    start_byte: u64,
    start_block: u64,
    start_block_offset: u64,
    logical_block_count: u64,
    block_size: usize,
    direction: Direction,
    limit: usize,
}

/// SearchLiteral 的实现：在【逻辑文件】上按字节模式搜索 `query`，
/// 从 `start_byte` 开始按 `direction` 方向扫描，返回最多 `limit` 个匹配的绝对偏移。
///
/// 与旧版（纯磁盘）不同：逐块读取时优先用缓存内容，因此未保存的编辑立即可被搜索到；
/// 块偏移由调用方经 delta_table 提供（start_block / start_block_offset），
/// 支持编辑后的非均匀块布局（含分裂新增块）。
///
/// 跨块匹配通过 overlap 缓冲处理：
/// - Forward：每块扫描时把上一块末尾 `query.len()-1` 字节拼到当前块开头
/// - Backward：每块扫描时把下一块开头 `query.len()-1` 字节拼到当前块末尾
async fn search_impl(
    file_path: &Path,
    cache: &BlockCache,
    wal: &Wal,
    args: SearchArgs,
) -> SearchResult {
    if args.query.is_empty() || args.limit == 0 || args.logical_block_count == 0 {
        return SearchResult { matches: vec![], scanned_blocks: 0, has_more_behind: false };
    }
    let disk_file_size = match std::fs::metadata(file_path) {
        Ok(m) => m.len(),
        Err(_) => return SearchResult { matches: vec![], scanned_blocks: 0, has_more_behind: false },
    };
    let overlap_len = args.query.len() - 1; // 安全：query 非空
    let start_block = args.start_block.min(args.logical_block_count.saturating_sub(1));

    match args.direction {
        Direction::Forward => {
            let mut matches: Vec<u64> = Vec::new();
            let mut has_more = false;
            let mut scanned = 0usize;
            let mut prev_overlap: Vec<u8> = Vec::new();
            // 块偏移从 start_block 的逻辑偏移起，随读取逐块累加（支持非均匀块大小）
            let mut block_offset: u64 = args.start_block_offset;

            for block_id in start_block..args.logical_block_count {
                scanned += 1;
                let content = logical_block_bytes(cache, Some(wal), None, file_path, block_id, args.block_size, disk_file_size).await.unwrap_or_default();

                let overlap_size = prev_overlap.len();
                let mut search_buf: Vec<u8> = Vec::with_capacity(overlap_size + content.len());
                search_buf.extend_from_slice(&prev_overlap);
                search_buf.extend_from_slice(&content);

                // 绝对偏移 = block_offset - overlap_size + pos（对 overlap 区与当前块都成立）
                for pos in memchr::memmem::find_iter(&search_buf, &args.query) {
                    let abs_signed = block_offset as i64 - overlap_size as i64 + pos as i64;
                    if abs_signed < 0 {
                        continue;
                    }
                    let abs_pos = abs_signed as u64;
                    if abs_pos < args.start_byte {
                        continue;
                    }
                    matches.push(abs_pos);
                    if matches.len() >= args.limit {
                        has_more = true;
                        break;
                    }
                }
                if matches.len() >= args.limit {
                    break;
                }

                // 保存当前块末尾的 overlap_len 字节作为下一块的前置 overlap
                if content.len() >= overlap_len {
                    prev_overlap = content[content.len() - overlap_len..].to_vec();
                } else {
                    prev_overlap = content.clone();
                }
                block_offset += content.len() as u64;
            }

            SearchResult { matches, scanned_blocks: scanned, has_more_behind: has_more }
        }
        Direction::Backward => {
            if args.start_byte == 0 {
                return SearchResult { matches: vec![], scanned_blocks: 0, has_more_behind: false };
            }
            let mut matches: Vec<u64> = Vec::new();
            let mut has_more = false;
            let mut scanned = 0usize;
            let mut next_overlap: Vec<u8> = Vec::new();
            // end 从 start_block 的块末逻辑偏移起向下递减，得到每块的起始偏移
            let mut end: u64 = args.start_block_offset;
            let mut first = true;

            for block_id in (0..=start_block).rev() {
                scanned += 1;
                let content = logical_block_bytes(cache, Some(wal), None, file_path, block_id, args.block_size, disk_file_size).await.unwrap_or_default();
                let content_len = content.len() as u64;
                if first {
                    end = args.start_block_offset + content_len;
                    first = false;
                }
                let block_start = end - content_len;
                let buf_len = content.len();

                let mut search_buf: Vec<u8> = Vec::with_capacity(buf_len + next_overlap.len());
                search_buf.extend_from_slice(&content);
                search_buf.extend_from_slice(&next_overlap);

                // 起点落在 next_overlap 区（pos >= buf_len）的匹配属于下一块，已在那块找过，跳过。
                let mut block_matches: Vec<u64> = Vec::new();
                for pos in memchr::memmem::find_iter(&search_buf, &args.query) {
                    if pos >= buf_len {
                        continue;
                    }
                    let abs_pos = block_start + pos as u64;
                    if abs_pos >= args.start_byte {
                        continue;
                    }
                    block_matches.push(abs_pos);
                }
                // 块内倒序：离 start_byte 近的在前
                block_matches.reverse();
                matches.extend(block_matches);

                if matches.len() >= args.limit {
                    matches.truncate(args.limit);
                    has_more = true;
                    break;
                }

                // 保存当前块开头的 overlap_len 字节作为上一块的后置 overlap
                if buf_len >= overlap_len {
                    next_overlap = content[..overlap_len].to_vec();
                } else {
                    next_overlap = content.clone();
                }
                end = block_start;
            }

            SearchResult { matches, scanned_blocks: scanned, has_more_behind: has_more }
        }
    }
}
