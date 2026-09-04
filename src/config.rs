use std::path::PathBuf;
use clap::ValueEnum;
use crate::EditError;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OpenMode {
    #[default]
    Auto,
    Text,
    Binary,
}

#[derive(Clone, Debug)]
pub struct EditorConfig {
    pub block_size: usize,
    pub mem_ratio: f32,
    pub prefetch_ahead: usize,
    pub prefetch_behind: usize,
    pub default_mode: OpenMode,
    pub undo_limit: usize,
    pub hex_bytes_per_row: u8,
    pub tab_width: u8,
    pub log_file: Option<PathBuf>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            block_size: 256 * 1024,
            mem_ratio: 0.3,
            prefetch_ahead: 4,
            prefetch_behind: 1,
            default_mode: OpenMode::Auto,
            undo_limit: 1000,
            hex_bytes_per_row: 16,
            tab_width: 4,
            log_file: None,
        }
    }
}

impl EditorConfig {
    /// 从 CLI 值合并 + 校验；mem_ratio > 0.7 -> cap to 0.7
    pub fn from_cli(block_size_kb: Option<u32>, mem_ratio: Option<f32>, mode: Option<OpenMode>) -> Result<Self, EditError> {
        let mut cfg = Self::default();
        if let Some(kb) = block_size_kb {
            if !(4..=16384).contains(&kb) {
                return Err(EditError::InvalidConfig("block_size 必须在 4KB ~ 16MB 之间".into()));
            }
            cfg.block_size = kb as usize * 1024;
        }
        if let Some(r) = mem_ratio {
            if !(0.05..=0.8).contains(&r) {
                return Err(EditError::InvalidConfig("mem_ratio 必须在 0.05 ~ 0.8 之间".into()));
            }
            cfg.mem_ratio = r;
        }
        if cfg.mem_ratio > 0.7 {
            cfg.mem_ratio = 0.7;
        }
        if let Some(m) = mode { cfg.default_mode = m; }
        Ok(cfg)
    }
}
