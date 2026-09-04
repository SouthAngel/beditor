use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum IoError {
    #[error("IO error: {0}")]
    StdIo(#[from] std::io::Error),
    #[error("块 {0} 越界 (总 {1} 块)")]
    BlockOutOfRange(u64, u64),
    #[error("短读: 期望 {expected}B, 只读 {actual}B")]
    ShortRead { expected: usize, actual: usize },
    #[error("保存失败: {0}")]
    SaveFailed(String),
    #[error("无权限写入: {0}")]
    PermissionDenied(PathBuf),
}

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("无法淘汰：所有块均为 dirty 或编辑中")]
    CannotEvict,
    #[error("块 {0} 被 pin，无法淘汰")]
    BlockPinned(u64),
}

#[derive(Error, Debug)]
pub enum EditError {
    #[error("偏移 {0} 超过文件大小 {1}")]
    OffsetOutOfRange(u64, u64),
    #[error("GapBuffer: {0}")]
    GapBuffer(String),
    #[error("撤销栈空")]
    NothingToUndo,
    #[error("重做栈空")]
    NothingToRedo,
    #[error("配置错误: {0}")]
    InvalidConfig(String),
}
