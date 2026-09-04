# beditor — 通用大文件终端编辑器

> **版本 0.1.0** | 支持 MB ~ 几十 GB 级文件的文本 + Hex 双模式编辑

基于 Rust (ratatui + crossterm + tokio) 构建，采用 **分块 Mmap 式架构 + LRU 缓存 + 每块 Gap Buffer**，
在有限内存占用下处理超大型文件。

---

## 特性

| 类别 | 说明 |
|------|------|
| **双模式** | 文本模式（自动检测 UTF-8 / GBK / GB18030 等多编码） + Hex 二进制模式（16 进制 + ASCII 预览），运行时可切换 |
| **内存管理** | 手写 LRU 缓存，容量按系统总内存比率配置（默认 30%，可调 5% ~ 80%，>70% 自动封顶） |
| **异步 IO** | Tokio workers 分块加载/预取/写回/搜索，TUI 零阻塞；支持在超大文件中即时响应 |
| **增量编辑** | 每块独立 Gap Buffer，跨块操作自动分裂/合并；完整 undo/redo 栈 |
| **原子保存** | 写临时文件 → fsync → 原子 rename，崩溃不丢数据；启动时检测遗留临时文件并告警 |
| **字面量搜索** | 文本/Hex 均支持前向/后向字面量搜索，自动处理跨块边界匹配（overlap 缓冲） |
| **类 Vim 交互** | NORMAL / INSERT / COMMAND / SEARCH 四模式，`:w :q :goto :mode :enc` 等命令 |
| **终端兼容** | 窄终端 (<40 列) Hex 模式自动降级为 8B/行，避免布局错乱 |
| **跨平台** | Windows / Linux / macOS，Rust edition 2021，`#![forbid(unsafe_code)]` |

---

## 架构

```
┌──────────────────────────────────────────────────────┐
│                        TUI (ratatui)                   │
│  ViewPort · LineIndex · render_text · render_hex · SB │
└───────────────────────────┬──────────────────────────┘
                            │ mpsc channel
┌───────────────────────────▼──────────────────────────┐
│                      Editor 核心                       │
│  cursor · insert/delete · undo/redo · split/merge     │
│  DeltaTable (FenwickTree) → 逻辑偏移 ↔ 块映射          │
└───────────────────────────┬──────────────────────────┘
                            │
┌───────────────────────────▼──────────────────────────┐
│                    BlockCache (手写 LRU)               │
│  pin / pending / Dirty·Clean / InGapBuffer 状态机      │
└───────────────────────────┬──────────────────────────┘
                            │ tokio mpsc + oneshot
┌───────────────────────────▼──────────────────────────┐
│                   IO Workers (tokio::spawn)            │
│  LoadBlock · PrefetchAhead · SearchLiteral · Flush    │
│  ReadFileHeader (编码检测) · save_as (原子 rename)    │
└───────────────────────────┬──────────────────────────┘
                            │ 256KB 分块
┌───────────────────────────▼──────────────────────────┐
│                   磁盘文件 (任意大小)                   │
└──────────────────────────────────────────────────────┘
```

### 关键数据结构

- **Block (256KB 默认)**：`GapBuffer` 实现块内高效插入删除；超过 1.5× 块大小自动分裂，低于 0.5× 自动合并
- **DeltaTable (Fenwick Tree)**：跟踪每块字节数变化，O(log n) 将逻辑偏移映射到 `(block_id, inner_pos)`
- **BlockCache (手写 LRU)**：编辑中的块 `pin` 住不被淘汰；Dirty 块写回前标记为 Clean；pending 请求合并避免重复 IO
- **LineIndex (LRU crate)**：文本模式按需扫描换行符，避免一次性全文件扫描

---

## 快速开始

### 安装与构建

需要 Rust 1.78+：

```bash
# 开发构建
cargo build

# 发布构建（LTO=fat，代码更小更快，约 2.5MB）
cargo build --release
```

### 命令行参数

```
beditor <FILE> [OPTIONS]

Arguments:
  <FILE>  要打开的文件路径

Options:
  -m, --mode <MODE>                 初始打开模式 [default: auto] [可能值: auto, text, hex]
      --block-size <BLOCK_SIZE_KB>  块大小 (KB) [default: 256]
      --mem-ratio <MEM_RATIO>       内存使用比率 (0.05 ~ 0.8) [default: 0.3]
  -h, --help                        打印帮助
```

### 使用示例

```bash
# 文本模式打开超大日志，50% 内存缓存
beditor access.log --mode text --mem-ratio 0.5

# Hex 模式编辑固件，64KB 小块（更快随机编辑）
beditor firmware.bin --mode hex --block-size 64

# 自动检测模式编辑 GB 级数据库文件，40% 内存
beditor large.db --mem-ratio 0.4
```

---

## 交互模式

### 四模式切换

| 模式 | 进入 | 退出 |
|------|------|------|
| **NORMAL** | 默认，或 INSERT/COMMAND 中按 `Esc` | — |
| **INSERT** | NORMAL 中按 `i` | `Esc` 返回 NORMAL |
| **COMMAND** | NORMAL 中按 `:` | `Enter` 执行 或 `Esc` 取消 |
| **SEARCH** | NORMAL 中按 `/` | `Enter` 执行 或 `Esc` 取消 |

### NORMAL 快捷键

| 按键 | 功能 |
|------|------|
| `h` / `←` | 光标左移 1 字节 |
| `l` / `→` | 光标右移 1 字节 |
| `j` / `↓` | 光标下移 1 行（文本模式估算 80B/行） |
| `k` / `↑` | 光标上移 1 行 |
| `^F` / `PageDown` | 下翻页（半屏估算） |
| `^B` / `PageUp` | 上翻页 |
| `g` | 跳到文件开头 |
| `G` | 跳到文件末尾 |
| `i` | 进入 INSERT 模式 |
| `x` | 删除光标下 1 字节 |
| `u` | 撤销 |
| `^R` | 重做 |
| `n` | 下一个搜索匹配 |
| `N` | 上一个搜索匹配 |
| `^C` | 无未保存修改则退出；否则提示 `:q!` 或 `:w` |

### COMMAND 指令

| 命令 | 说明 |
|------|------|
| `:w` | 保存（原子 rename） |
| `:w <path>` | 另存为指定路径 |
| `:q` | 退出（有未保存修改拒绝） |
| `:q!` | 强制退出 |
| `:wq` / `:x` | 保存并退出 |
| `:goto <offset>` | 跳到指定字节偏移（支持 `1M`/`2G`？见注） |
| `:mode text` / `:mode hex` | 切换显示模式 |
| `:enc utf-8` / `:enc gbk` / `:enc gb18030` | 切换文本编码 |
| `:info` | 显示文件/块/缓存/命中统计 |
| `:h` / `:help` | 帮助 |

> 注：当前版本 `:goto` 接受十进制字节偏移。

### SEARCH

| 操作 | 说明 |
|------|------|
| `/query Enter` | 从光标位置向前字面量搜索 query |
| `n` | 跳到下一个匹配 |
| `N` | 跳到上一个匹配 |

文本模式中 query 按 UTF-8 字符匹配；Hex 模式中按 ASCII 字面量匹配字节序列。

---

## 开发

### 项目结构

```
Cargo.toml
src/
  main.rs          # CLI + 主循环 (tokio select!)
  lib.rs           # 模块入口
  config.rs        # EditorConfig / OpenMode
  error.rs         # thiserror 分层错误
  message.rs       # IoRequest / IoEvent / 通道消息
  block.rs         # BlockId · BlockState · BlockData · GapBuffer
  delta_table.rs   # FenwickTree + DeltaTable（偏移映射）
  cache.rs         # 手写 LRU BlockCache + pin/pending
  encoding.rs      # chardetng + encoding_rs 多编码检测
  io_worker.rs     # 5 类异步 IO worker
  editor.rs        # Editor 核心 + insert/delete/undo/redo/save/search
  command.rs       # 输入模式 + 命令解析
  tui/
    mod.rs         # init_tui / restore_tui / render 路由
    viewport.rs    # ViewPort + LineIndex + 滚动
    render_text.rs # 文本模式渲染
    render_hex.rs  # Hex 模式渲染
    status_bar.rs  # 状态栏 + MiniBuffer
tests/             # 集成测试 + 单元测试 (74 cases)
benches/           # GapBuffer + BlockCache 性能基准
```

### 测试

```bash
# 全部测试
cargo test

# 指定模块
cargo test --test edit_flow        # 端到端编辑流 + 保存一致性
cargo test --test cross_block      # 块边界分裂合并
cargo test --test search_cross_block  # 跨块搜索匹配
cargo test --test delta_table      # DeltaTable / locate_offset
```

### Clippy

```bash
cargo clippy --all-targets -- -D warnings
```

### 性能基准

```bash
# GapBuffer 插入/删除/as_contiguous
# BlockCache try_get/insert/pin/take
cargo bench
```

典型数值（release，消费级 CPU）：

| 基准 | 结果 |
|------|------|
| `insert_1KB_sequential` | ~220 ns/op |
| `insert_1byte_mid_256KB` | ~20 µs/op |
| `delete_1KB_mid_256KB` | ~20 µs/op |
| `cache_hit try_get` | ~90 ns/op |
| `cache_miss try_get` | ~18 ns/op |
| `pin_for_edit_cycle` | ~75 ns/op |

---

## 设计约束

- **块大小约束**：`:w` 保存时 Clean 块直接从原文件零拷贝读（大 buf，等价 copy_file_range 思路），Dirty 块从缓存写回。
- **崩溃安全**：保存过程中崩溃仅留下 `<file>.beditor-tmp` 临时文件，原文件完好；下次启动自动告警。
- **已知限制**：v0.1.0 暂不支持跨块 delete，编辑超过块边界的删除操作将返回 `跨块删除在 Task9 实现` 错误（仅限极罕见的 exact 块边界场景）。

---

## License

MIT
