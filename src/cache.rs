use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use tokio::sync::oneshot;
use crate::block::{Block, BlockData, BlockId, BlockState, GapBuffer};
use crate::config::EditorConfig;
use crate::error::{CacheError, EditError};
use crate::wal::Wal;

/// Block 的只读快照（渲染和大部分读操作使用，避免持锁）
#[derive(Clone, Debug)]
pub struct BlockSnapshot {
    pub id: BlockId,
    pub raw_len: usize,
    pub len: usize,
    pub state: BlockState,
    pub contiguous: Vec<u8>,
}

impl From<&Block> for BlockSnapshot {
    fn from(b: &Block) -> Self {
        let contiguous = match &b.data {
            BlockData::Raw(v) => v.clone(),
            BlockData::Gap(g) => g.as_contiguous(),
        };
        Self { id: b.id, raw_len: b.raw_len, len: b.len(), state: b.state, contiguous }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub size_blocks: usize,
    pub capacity_blocks: usize,
    pub pinned_blocks: usize,
}

pub enum GetOutcome {
    Hit(BlockSnapshot),
    MissNeedLoad(oneshot::Receiver<BlockSnapshot>),
}

/// 数据区：块内容 + pin 计数 + pending 等待者。读路径（try_get）只取读锁。
struct Inner {
    map: HashMap<BlockId, Block>,
    pinned: HashMap<BlockId, usize>,
    pending: HashMap<BlockId, Vec<oneshot::Sender<BlockSnapshot>>>,
}

/// LRU 顺序区：独立小锁。读路径更新访问顺序只需这把锁，
/// 不再需要把整个数据区加写锁，避免高频渲染互斥。
struct LruInner {
    /// LRU：头=最近使用，尾=最久未用；pinned 块不出现在此列表
    lru: VecDeque<BlockId>,
    capacity: usize,
}

pub struct BlockCache {
    inner: RwLock<Inner>,
    lru: Mutex<LruInner>,
    capacity_blocks: usize,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub evictions: AtomicU64,
    #[allow(dead_code)]
    config: Arc<EditorConfig>,
    /// 增量持久化 WAL：脏块被淘汰前写入，防止内存压力丢失未保存编辑
    wal: Mutex<Option<Arc<Wal>>>,
}

impl BlockCache {
    pub fn new(config: Arc<EditorConfig>, capacity_blocks: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                map: HashMap::new(),
                pinned: HashMap::new(),
                pending: HashMap::new(),
            }),
            lru: Mutex::new(LruInner {
                lru: VecDeque::new(),
                capacity: capacity_blocks,
            }),
            capacity_blocks,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            config,
            wal: Mutex::new(None),
        }
    }

    /// 挂接 WAL（由 io_worker 在创建缓存后调用）
    pub fn attach_wal(&self, wal: Arc<Wal>) {
        *self.wal.lock() = Some(wal);
    }

    /// 当前挂接的 WAL（可能为 None）
    pub fn wal(&self) -> Option<Arc<Wal>> {
        self.wal.lock().clone()
    }

    /// 测试专用：绕过 sysinfo 直接定容量
    pub fn new_for_test(config: Arc<EditorConfig>, capacity_blocks: usize) -> Self {
        Self::new(config, capacity_blocks)
    }

    pub fn stats(&self) -> CacheStats {
        let i = self.inner.read();
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            size_blocks: i.map.len(),
            capacity_blocks: self.capacity_blocks,
            pinned_blocks: i.pinned.len(),
        }
    }

    pub fn try_get(&self, id: BlockId) -> Option<BlockSnapshot> {
        let i = self.inner.read();
        if !i.map.contains_key(&id) {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.hits.fetch_add(1, Ordering::Relaxed);
        // 更新 LRU 访问顺序（独立小锁），读路径不再持有写锁
        let mut l = self.lru.lock();
        if !i.pinned.contains_key(&id) {
            if let Some(pos) = l.lru.iter().position(|x| *x == id) {
                l.lru.remove(pos);
            }
            l.lru.push_front(id);
        }
        drop(l);
        let b = i.map.get(&id).expect("existence checked above");
        Some(BlockSnapshot::from(b))
    }

    pub fn get_or_register_pending(&self, id: BlockId) -> GetOutcome {
        if let Some(s) = self.try_get(id) {
            return GetOutcome::Hit(s);
        }
        let mut i = self.inner.write();
        if let Some(b) = i.map.get(&id) {
            return GetOutcome::Hit(BlockSnapshot::from(b));
        }
        let (tx, rx) = oneshot::channel();
        i.pending.entry(id).or_default().push(tx);
        GetOutcome::MissNeedLoad(rx)
    }

    pub fn insert_loaded(&self, block: Block) {
        let id = block.id;
        let snap = BlockSnapshot::from(&block);
        let mut pending_receivers = Vec::new();
        // 被淘汰的脏块在释放锁后写入 WAL（避免持锁做文件 I/O）
        let mut dirty_flush: Vec<(BlockId, Vec<u8>)> = Vec::new();
        {
            let mut i = self.inner.write();
            if let Some(v) = i.pending.remove(&id) {
                pending_receivers = v;
            }
            if i.map.contains_key(&id) {
                i.map.insert(id, block);
            } else {
                let mut l = self.lru.lock();
                while i.map.len() >= l.capacity {
                    let Some(vid) = l.lru.pop_back() else { break };
                    if i.pinned.contains_key(&vid) {
                        continue;
                    }
                    if let Some(b) = i.map.remove(&vid) {
                        self.evictions.fetch_add(1, Ordering::Relaxed);
                        // 脏块被淘汰前必须持久化，否则内存压力会丢失未保存编辑
                        if matches!(b.state, BlockState::Dirty | BlockState::InGapBuffer) {
                            dirty_flush.push((vid, b.into_contiguous_bytes()));
                        }
                    }
                }
                i.map.insert(id, block);
                l.lru.push_front(id);
                drop(l);
            }
        }
        if !dirty_flush.is_empty() {
            if let Some(wal) = self.wal.lock().clone() {
                if let Err(e) = wal.append_many(&dirty_flush) {
                    // 磁盘故障属于严重错误；WAL 写入失败不能悄悄吞掉，
                    // 但此处无通道上报，先记录最坏情况：不重新入缓存（内容已取走）。
                    eprintln!("WAL 写入失败，脏块可能丢失: {e}");
                }
            }
        }
        for tx in pending_receivers {
            let _ = tx.send(snap.clone());
        }
    }

    pub fn pin_for_edit(&self, id: BlockId) -> Result<EditGuard<'_>, CacheError> {
        let mut i = self.inner.write();
        // 从 LRU 移除（进入 pinned 集合，不可被淘汰）
        self.lru.lock().lru.retain(|x| *x != id);
        *i.pinned.entry(id).or_insert(0) += 1;
        let block = i.map.get_mut(&id).ok_or(CacheError::BlockPinned(id))?;
        let _ = block.to_gap();
        block.state = BlockState::InGapBuffer;
        Ok(EditGuard { cache: self, block_id: id })
    }

    pub fn with_pin_mut<F, R>(&self, id: BlockId, f: F) -> Result<R, EditError>
    where
        F: FnOnce(&mut GapBuffer) -> Result<R, EditError>,
    {
        let mut i = self.inner.write();
        let block = i.map.get_mut(&id).ok_or(EditError::OffsetOutOfRange(id, 0))?;
        let gb = match &mut block.data {
            BlockData::Gap(g) => g,
            _ => return Err(EditError::GapBuffer("block not in gap mode".into())),
        };
        f(gb)
    }

    pub fn dirty_ids(&self) -> Vec<BlockId> {
        let i = self.inner.read();
        i.map
            .iter()
            .filter(|(_, b)| matches!(b.state, BlockState::Dirty | BlockState::InGapBuffer))
            .map(|(k, _)| *k)
            .collect()
    }

    pub fn take_contiguous_and_mark_clean(&self, id: BlockId) -> Option<Vec<u8>> {
        let mut i = self.inner.write();
        let b = i.map.get_mut(&id)?;
        let bytes = match &b.data {
            BlockData::Raw(v) => v.clone(),
            BlockData::Gap(g) => g.as_contiguous(),
        };
        let new_len = bytes.len();
        b.data = BlockData::Raw(bytes.clone());
        b.raw_len = new_len;
        b.state = BlockState::Clean;
        Some(bytes)
    }

    /// 把 `removed` 块的字节追加到 `target` 块末尾，并从缓存移除 `removed`。
    /// 用于 undo Split（分裂回退）：恢复分裂前的块内容。
    pub fn merge_blocks(&self, target: BlockId, removed: BlockId) -> Result<(), EditError> {
        let mut i = self.inner.write();
        let removed_bytes = {
            let rb = i.map.get(&removed)
                .ok_or_else(|| EditError::GapBuffer(format!("合并块 {removed} 不在缓存")))?;
            match &rb.data {
                BlockData::Raw(v) => v.clone(),
                BlockData::Gap(g) => g.as_contiguous(),
            }
        };
        {
            let tb = i.map.get_mut(&target)
                .ok_or_else(|| EditError::GapBuffer(format!("合并目标块 {target} 不在缓存")))?;
            let _ = tb.to_gap();
            if let BlockData::Gap(g) = &mut tb.data {
                g.insert(g.len(), &removed_bytes)?;
            }
            tb.state = BlockState::InGapBuffer;
        }
        // 从 LRU 与数据区移除 removed
        self.lru.lock().lru.retain(|x| *x != removed);
        i.map.remove(&removed);
        i.pinned.remove(&removed);
        Ok(())
    }

    /// 把 `block_id` 在 `split_at` 处切成两块：前半留在原块，后半成为新块
    /// （id = block_id + 1，覆盖可能残留的同 id 旧块）。
    /// 返回新块的字节数，供 delta_table 记录大小。
    pub fn split_block(&self, block_id: BlockId, split_at: usize) -> Result<usize, EditError> {
        let mut i = self.inner.write();
        let right_bytes = {
            let b = i.map.get_mut(&block_id)
                .ok_or_else(|| EditError::GapBuffer(format!("分裂目标块 {block_id} 不在缓存")))?;
            let _ = b.to_gap();
            let gb = match &mut b.data {
                BlockData::Gap(g) => g,
                _ => unreachable!("to_gap 后必为 Gap"),
            };
            let right = gb.split_at(split_at)?;
            right.as_contiguous()
        };
        let new_id = block_id + 1;
        // 覆盖可能残留的旧块（防御性清理）
        self.lru.lock().lru.retain(|x| *x != new_id);
        i.map.insert(new_id, Block::new_clean(new_id, right_bytes.clone()));
        self.lru.lock().lru.push_front(new_id);
        Ok(right_bytes.len())
    }
}

/// RAII guard：drop 时自动 unpin + 标记 Dirty
pub struct EditGuard<'a> {
    cache: &'a BlockCache,
    block_id: BlockId,
}

impl<'a> Drop for EditGuard<'a> {
    fn drop(&mut self) {
        let mut i = self.cache.inner.write();
        if let Some(cnt) = i.pinned.get_mut(&self.block_id) {
            *cnt -= 1;
            if *cnt == 0 {
                i.pinned.remove(&self.block_id);
                let mut l = self.cache.lru.lock();
                if !l.lru.contains(&self.block_id) {
                    // 刚 unpin 的 Dirty 块放在 LRU 尾部（最旧），优先淘汰以便写回
                    l.lru.push_back(self.block_id);
                }
                drop(l);
                if let Some(b) = i.map.get_mut(&self.block_id) {
                    if matches!(b.state, BlockState::InGapBuffer) {
                        b.state = BlockState::Dirty;
                    }
                }
            }
        }
    }
}
