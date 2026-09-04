#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod message;
pub mod block;
pub mod delta_table;
pub mod cache;
pub mod encoding;
pub mod io_worker;
pub mod editor;
pub mod command;
pub mod tui;
pub mod wal;
pub mod line_index;

pub use config::{EditorConfig, OpenMode};
pub use error::{EditError, IoError, CacheError};
pub use message::{Direction, IoEvent, IoRequest, SaveReport, SearchResult};
// TODO(task2): pub use block::{Block, BlockData, BlockId, BlockState, GapBuffer};
// TODO(task3): pub use delta_table::DeltaTable;
// TODO(task4): pub use cache::BlockCache;
