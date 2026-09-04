//! 日志式 WAL（Write-Ahead Log）：增量持久化层。
//!
//! 基础文件始终保持**编辑前**的原始字节（块 i 位于 i*block_size，可直接按块读取），
//! 所有编辑产生的脏块以追加方式写入 `<file>.beditor-wal`，从而：
//! - `:w` 只需追加脏块 + fsync（O(脏块)，而非 O(文件)）
//! - 崩溃时 WAL 可回放，未保存编辑不丢失
//! - 脏块从 LRU 淘汰前写入 WAL，避免内存压力丢数据
//!
//! 文件格式（小端）：`[block_id: u32][len: u32][bytes: len]`，逐条追加。
//! 同 block_id 多条时后者覆盖前者（读取时取最新）。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub type BlockId = u64;

struct WalEntry {
    offset: u64,
    len: u32,
}

struct WalInner {
    file: std::fs::File,
    index: HashMap<BlockId, WalEntry>,
    end: u64,
}

pub struct Wal {
    #[allow(dead_code)]
    path: PathBuf,
    inner: Mutex<WalInner>,
}

impl Wal {
    /// 打开/创建 WAL 文件并重建索引（用于崩溃恢复回放）。
    /// 若文件尾部有半截条目（写入中途崩溃），忽略该半截条目。
    pub fn open(path: PathBuf) -> std::io::Result<Arc<Wal>> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false) // 保留既有条目，供崩溃回放
            .open(&path)?;
        let mut index = HashMap::new();
        let mut end: u64 = 0;
        loop {
            let mut hdr = [0u8; 8];
            match file.read_exact(&mut hdr) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break, // 尾部半截/EOF
                Err(e) => return Err(e),
            }
            let id = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
            let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
            let mut bytes = vec![0u8; len];
            if let Err(e) = file.read_exact(&mut bytes) {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break; // 数据不完整，忽略该条
                }
                return Err(e);
            }
            index.insert(id, WalEntry { offset: end + 8, len: len as u32 });
            end += 8 + len as u64;
        }
        Ok(Arc::new(Wal {
            path,
            inner: Mutex::new(WalInner { file, index, end }),
        }))
    }

    /// 追加一批脏块并 fsync。批量写 + 一次 fsync，减少落盘次数。
    pub fn append_many(&self, entries: &[(BlockId, Vec<u8>)]) -> std::io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.lock().unwrap();
        let mut offset = inner.end;
        for (id, bytes) in entries {
            inner.file.seek(SeekFrom::Start(offset))?;
            inner.file.write_all(&(*id as u32).to_le_bytes())?;
            inner.file.write_all(&(bytes.len() as u32).to_le_bytes())?;
            inner.file.write_all(bytes)?;
            // 记录数据区起始偏移（跳过 8 字节头），供 get 直接读取
            inner.index.insert(*id, WalEntry { offset: offset + 8, len: bytes.len() as u32 });
            offset += 8 + bytes.len() as u64;
        }
        inner.file.sync_all()?;
        inner.end = offset;
        Ok(())
    }

    /// 追加单个脏块并 fsync。
    pub fn append_one(&self, id: BlockId, bytes: &[u8]) -> std::io::Result<()> {
        self.append_many(&[(id, bytes.to_vec())])
    }

    /// 读取某个块在 WAL 中的最新内容；不存在返回 None。
    pub fn get(&self, id: BlockId) -> std::io::Result<Option<Vec<u8>>> {
        let mut inner = self.inner.lock().unwrap();
        let Some(e) = inner.index.get(&id) else {
            return Ok(None);
        };
        let (offset, len) = (e.offset, e.len as usize);
        inner.file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; len];
        inner.file.read_exact(&mut buf)?;
        Ok(Some(buf))
    }

    /// 是否存在未合并的条目（打开时用于提示/退出前合并）
    pub fn has_entries(&self) -> bool {
        !self.inner.lock().unwrap().index.is_empty()
    }

    /// 当前条目数
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().index.len()
    }

    /// 是否没有条目
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().index.is_empty()
    }

    /// 合并（checkpoint）后清空 WAL，恢复到无增量状态。
    pub fn reset(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.file.set_len(0)?;
        inner.file.seek(SeekFrom::Start(0))?;
        inner.file.sync_all()?;
        inner.index.clear();
        inner.end = 0;
        Ok(())
    }
}
