use crate::EditError;

pub type BlockId = u64;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlockState {
    Clean,
    Dirty,
    InGapBuffer,
}

pub enum BlockData {
    Raw(Vec<u8>),
    Gap(GapBuffer),
}

pub struct Block {
    pub id: BlockId,
    pub raw_len: usize,
    pub data: BlockData,
    pub state: BlockState,
}

impl Block {
    pub fn new_clean(id: BlockId, bytes: Vec<u8>) -> Self {
        let raw_len = bytes.len();
        Self {
            id,
            raw_len,
            data: BlockData::Raw(bytes),
            state: BlockState::Clean,
        }
    }

    pub fn len(&self) -> usize {
        match &self.data {
            BlockData::Raw(v) => v.len(),
            BlockData::Gap(g) => g.len(),
        }
    }

    pub fn is_empty(&self) -> bool { self.len() == 0 }

    /// 把该块转为 GapBuffer（用于进入编辑态）；重复调用无副作用
    pub fn to_gap(&mut self) -> &mut GapBuffer {
        if matches!(self.data, BlockData::Raw(_)) {
            let owned = std::mem::replace(&mut self.data, BlockData::Gap(GapBuffer::new(vec![])));
            if let BlockData::Raw(v) = owned {
                self.data = BlockData::Gap(GapBuffer::new(v));
            }
        }
        match &mut self.data {
            BlockData::Gap(g) => g,
            _ => unreachable!(),
        }
    }

    /// 把 GapBuffer 合并成连续 Vec<u8>（写回磁盘用）
    pub fn into_contiguous_bytes(self) -> Vec<u8> {
        match self.data {
            BlockData::Raw(v) => v,
            BlockData::Gap(g) => g.as_contiguous(),
        }
    }
}

pub struct GapBuffer {
    buf: Vec<u8>,
    gap_start: usize,
    gap_end: usize,
    pub original_len: usize,
}

impl GapBuffer {
    pub fn new(initial: Vec<u8>) -> Self {
        let original_len = initial.len();
        let cap = (initial.len() * 2).max(16);
        let mut buf = Vec::with_capacity(cap);
        buf.extend_from_slice(&initial);
        let gap_start = buf.len();
        buf.resize(buf.capacity(), 0);
        let gap_end = buf.len();
        Self { buf, gap_start, gap_end, original_len }
    }

    pub fn len(&self) -> usize {
        self.gap_start + (self.buf.len() - self.gap_end)
    }

    pub fn is_empty(&self) -> bool { self.len() == 0 }

    fn grow_gap(&mut self, need_additional: usize) {
        let real = self.len();
        let new_total_cap = ((real + need_additional) * 3 / 2).max(real + need_additional + 16);
        let mut new_buf = vec![0u8; new_total_cap];
        new_buf[..self.gap_start].copy_from_slice(&self.buf[..self.gap_start]);
        let suffix_len = self.buf.len() - self.gap_end;
        let new_gap_end = new_total_cap - suffix_len;
        new_buf[new_gap_end..].copy_from_slice(&self.buf[self.gap_end..]);
        self.buf = new_buf;
        self.gap_end = new_gap_end;
    }

    pub fn insert(&mut self, pos: usize, bytes: &[u8]) -> Result<(), EditError> {
        if pos > self.len() {
            return Err(EditError::GapBuffer(format!("insert pos {pos} out of bounds (len {})", self.len())));
        }
        self.move_gap_to(pos);
        if bytes.len() > self.gap_end - self.gap_start {
            self.grow_gap(bytes.len());
        }
        let dst = &mut self.buf[self.gap_start..self.gap_start + bytes.len()];
        dst.copy_from_slice(bytes);
        self.gap_start += bytes.len();
        Ok(())
    }

    pub fn delete(&mut self, pos: usize, len: usize) -> Result<(), EditError> {
        if len == 0 { return Ok(()); }
        if pos + len > self.len() {
            return Err(EditError::GapBuffer(
                format!("delete range out of bounds: {pos}..{} (len {})", pos + len, self.len())
            ));
        }
        self.move_gap_to(pos);
        self.gap_end += len;
        Ok(())
    }

    pub fn slice(&self, range: std::ops::Range<usize>) -> Result<Vec<u8>, EditError> {
        let std::ops::Range { start, end } = range;
        if end > self.len() || start > end {
            return Err(EditError::GapBuffer(format!("slice {start}..{end} out of len {}", self.len())));
        }
        let mut out = Vec::with_capacity(end - start);
        let prefix_take = (self.gap_start).min(end).saturating_sub(start);
        if prefix_take > 0 {
            out.extend_from_slice(&self.buf[start..start + prefix_take]);
        }
        let remain = (end - start) - prefix_take;
        if remain > 0 {
            let suffix_logic_start = (start + prefix_take).saturating_sub(self.gap_start);
            let suffix_real_start = self.gap_end + suffix_logic_start;
            out.extend_from_slice(&self.buf[suffix_real_start..suffix_real_start + remain]);
        }
        Ok(out)
    }

    pub fn as_contiguous(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        out.extend_from_slice(&self.buf[..self.gap_start]);
        out.extend_from_slice(&self.buf[self.gap_end..]);
        out
    }

    pub fn split_at(&mut self, mid: usize) -> Result<Self, EditError> {
        if mid > self.len() {
            return Err(EditError::GapBuffer(format!("split_at {mid} > len {}", self.len())));
        }
        let right_bytes = self.slice(mid..self.len())?;
        self.delete(mid, self.len() - mid)?;
        Ok(Self::new(right_bytes))
    }

    fn move_gap_to(&mut self, pos: usize) {
        if pos == self.gap_start { return; }
        if pos < self.gap_start {
            let move_len = self.gap_start - pos;
            let new_gs = pos;
            let new_ge = self.gap_end - move_len;
            self.buf.copy_within(pos..self.gap_start, new_ge);
            self.gap_start = new_gs;
            self.gap_end = new_ge;
        } else {
            let move_len = pos - self.gap_start;
            let suffix_start = self.gap_end;
            self.buf.copy_within(suffix_start..suffix_start + move_len, self.gap_start);
            self.gap_start += move_len;
            self.gap_end += move_len;
        }
    }
}
