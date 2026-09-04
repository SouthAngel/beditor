use std::path::Path;
use chardetng::EncodingDetector;
use encoding_rs::Encoding;
use tracing::warn;
use crate::config::OpenMode;

/// 返回：confidence 高的编码（chardetng 自动判断），兜底 UTF-8
pub fn detect_encoding(header: &[u8]) -> &'static Encoding {
    let mut det = EncodingDetector::new();
    det.feed(header, false);
    let (enc, confident) = det.guess_assess(None, true);
    if !confident {
        warn!("编码检测置信度低，猜测 {}，可手动用 :enc 切换", enc.name());
    }
    enc
}

const TEXT_EXTS: &[&str] = &[
    "txt", "md", "log", "csv", "json", "yaml", "yml", "toml", "ini", "cfg", "conf",
    "rs", "go", "py", "js", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "java", "rb",
    "php", "sh", "bash", "ps1", "bat", "sql", "html", "htm", "css", "scss", "less",
    "xml", "svg", "lrc", "srt", "diff", "patch", "rst", "tex",
];

const BINARY_EXTS: &[&str] = &[
    "exe", "dll", "so", "dylib", "bin", "o", "a", "lib", "elf", "pe",
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "ico", "mp3", "mp4", "flac",
    "wav", "avi", "mkv", "mov", "pdf", "zip", "tar", "gz", "bz2", "xz", "7z",
    "rar", "class", "jar", "wasm", "pak", "dat", "db", "sqlite",
];

/// 返回 (mode, encoding)；当 mode=Binary 时 encoding 无意义
pub fn detect_mode(
    header: &[u8],
    file_path: &Path,
    default: OpenMode,
) -> (OpenMode, &'static Encoding) {
    let enc = detect_encoding(header);
    match default {
        OpenMode::Text => return (OpenMode::Text, enc),
        OpenMode::Binary => return (OpenMode::Binary, enc),
        OpenMode::Auto => {}
    }
    // 规则1: NUL 字节直接判定 binary
    if header.iter().take(4096).any(|b| *b == 0x00) {
        return (OpenMode::Binary, enc);
    }
    // 规则2: 扩展名优先
    if let Some(ext) = file_path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()) {
        if BINARY_EXTS.contains(&ext.as_str()) {
            return (OpenMode::Binary, enc);
        }
        if TEXT_EXTS.contains(&ext.as_str()) {
            return (OpenMode::Text, enc);
        }
    }
    // 规则3: 未知扩展名 + 不含 NUL → 判文本
    (OpenMode::Text, enc)
}

pub fn decode(bytes: &[u8], enc: &'static Encoding) -> String {
    let (cow, _had_errors, _used_encoding) = enc.decode(bytes);
    cow.into_owned()
}

pub fn encode(s: &str, enc: &'static Encoding) -> Vec<u8> {
    let (cow, _used_encoding, _had_errors) = enc.encode(s);
    cow.into_owned()
}
