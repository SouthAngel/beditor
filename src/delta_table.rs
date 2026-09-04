use crate::block::BlockId;

/// 点更新 + 前缀和查询 O(log n) 的 Fenwick Tree
/// 存储每个块的实际字节数（size），prefix_sum(i) = 前 i 个块的总字节数
struct FenwickTree<T: Copy + std::ops::AddAssign + Default + std::ops::Sub<Output = T>> {
    tree: Vec<T>,
}

impl<T> FenwickTree<T>
where
    T: Copy + std::ops::AddAssign + Default + std::ops::Sub<Output = T>,
{
    fn new(n: usize) -> Self {
        Self { tree: vec![T::default(); n + 1] }
    }

    fn len(&self) -> usize { self.tree.len().saturating_sub(1) }

    fn add(&mut self, mut idx: usize, delta: T) {
        debug_assert!(idx < self.len(), "Fenwick idx out of bounds");
        idx += 1;
        while idx < self.tree.len() {
            self.tree[idx] += delta;
            idx += idx & idx.wrapping_neg();
        }
    }

    fn prefix_sum(&self, idx: usize) -> T {
        let idx = idx.min(self.len());
        let mut s = T::default();
        let mut i = idx;
        while i > 0 {
            s += self.tree[i];
            i -= i & i.wrapping_neg();
        }
        s
    }

    fn get(&self, idx: usize) -> T {
        self.prefix_sum(idx + 1) - self.prefix_sum(idx)
    }

    fn insert(&mut self, idx: usize, val: T) {
        let mut v: Vec<T> = (0..self.len()).map(|i| self.get(i)).collect();
        v.insert(idx, val);
        *self = Self::from_vec(&v);
    }

    fn remove(&mut self, idx: usize) -> T {
        let v: Vec<T> = (0..self.len()).map(|i| self.get(i)).collect();
        let removed = v[idx];
        let mut new_v = v;
        new_v.remove(idx);
        *self = Self::from_vec(&new_v);
        removed
    }

    fn from_vec(v: &[T]) -> Self {
        let mut s = Self::new(v.len());
        for (i, x) in v.iter().enumerate() {
            s.add(i, *x);
        }
        s
    }
}

pub struct DeltaTable {
    /// Fenwick tree 存储每个块的实际字节数（size）
    tree: FenwickTree<i64>,
    block_count: u64,
    /// 每个块的原始字节数（含部分块），用于计算 block_delta
    original_sizes: Vec<i64>,
}

impl DeltaTable {
    pub fn new(initial_block_count: u64, block_size: usize, file_size: u64) -> Self {
        let n = initial_block_count as usize;
        let bs = block_size as i64;
        let mut sizes = vec![bs; n];
        // 最后一块可能不足 block_size：用实际字节数修正
        if n > 0 {
            let last_block_size = (file_size as i64) - ((n as i64) - 1) * bs;
            if last_block_size > 0 && last_block_size < bs {
                sizes[n - 1] = last_block_size;
            }
        }
        let original_sizes = sizes.clone();
        Self {
            tree: FenwickTree::from_vec(&sizes),
            block_count: initial_block_count,
            original_sizes,
        }
    }

    /// 记录指定块的字节变化量（绝对值：该块当前总变化量 = size - block_size）
    /// 记录指定块的字节变化量。delta = 该块当前相对原始大小的变化量。
    pub fn record_delta(&mut self, block_id: BlockId, delta: i64) {
        debug_assert!((block_id as usize) < self.tree.len(), "record_delta block_id out of range");
        let orig = self.original_sizes[block_id as usize];
        let new_size = orig + delta;
        let old_size = self.tree.get(block_id as usize);
        let diff = new_size - old_size;
        if diff != 0 {
            self.tree.add(block_id as usize, diff);
        }
    }

    /// 在 after 块之后插入一个新块（逻辑id = after+1）
    /// initial_delta 为新块的初始字节大小
    pub fn insert_block_after(&mut self, after: BlockId, initial_delta: i64) {
        let insert_at = (after + 1) as usize;
        self.tree.insert(insert_at, initial_delta);
        self.original_sizes.insert(insert_at, initial_delta);
        self.block_count += 1;
    }

    /// 合并相邻块：removed = target+1
    /// target 吸收 removed 的全部字节，removed 从树中移除
    pub fn merge_block(&mut self, target: BlockId, removed: BlockId) {
        assert!(removed == target + 1, "merge_block requires removed == target + 1");
        let removed_size = self.tree.remove(removed as usize);
        // 合并后 target 的原始大小 = 两者原始大小之和
        let merged_orig = self.original_sizes[target as usize] + self.original_sizes[removed as usize];
        self.original_sizes.remove(removed as usize);
        self.original_sizes[target as usize] = merged_orig;
        self.tree.add(target as usize, removed_size);
        self.block_count -= 1;
    }

    /// 查询逻辑块 block_id 在当前文件中的字节起始偏移
    /// = 前 block_id 个块的字节数之和
    pub fn block_start_offset(&self, block_id: BlockId, _block_size: usize) -> u64 {
        let acc: i64 = self.tree.prefix_sum(block_id as usize);
        debug_assert!(acc >= 0, "block_start_offset underflow: {acc}");
        acc.max(0) as u64
    }

    /// 整个文件的累计 delta（当前总字节数 - 原始总字节数）
    pub fn total_delta(&self) -> i64 {
        let total_size = self.tree.prefix_sum(self.tree.len());
        let original_total: i64 = self.original_sizes.iter().sum();
        total_size - original_total
    }

    /// 单个块的 delta（当前字节数 - 原始字节数）
    pub fn block_delta(&self, id: BlockId) -> i64 {
        self.tree.get(id as usize) - self.original_sizes[id as usize]
    }

    /// 当前（编辑后）某块的字节数
    pub fn block_size(&self, id: BlockId) -> i64 {
        self.tree.get(id as usize)
    }

    pub fn block_count(&self) -> u64 { self.block_count }

    /// 将【逻辑字节偏移】映射到 (block_id, inner_pos)。
    ///
    /// 编辑后的逻辑布局不再是块大小整数倍对齐，必须用 DeltaTable 的累计前缀和
    /// 定位 cursor_byte 落在哪个块、块内偏移多少。O(log n)。
    pub fn locate_offset(&self, offset: u64) -> (BlockId, usize) {
        let n = self.tree.len();
        if n == 0 {
            return (0, 0);
        }
        // 二分：找最大的 b 使得 prefix_sum(b) <= offset
        let mut lo: usize = 0;
        let mut hi: usize = n;
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            let acc = self.tree.prefix_sum(mid);
            if acc as u64 <= offset {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // lo 现在是 cursor 所在块的下标（块大小至少 1 字节时成立）
        // 边界：offset 超出所有块总大小 → 归到最后一块的末尾
        let block_start = self.tree.prefix_sum(lo) as u64;
        let block_size = self.tree.get(lo) as u64;
        let inner = offset.saturating_sub(block_start).min(block_size);
        (lo as BlockId, inner as usize)
    }
}
