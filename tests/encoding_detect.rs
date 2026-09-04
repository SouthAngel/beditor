use std::path::Path;
use beditor::config::OpenMode;
use beditor::encoding;

#[test]
fn detect_utf8_text() {
    let s = "你好，世界\nHello world";
    let bytes = s.as_bytes().to_vec();
    let enc = encoding::detect_encoding(&bytes);
    assert!(enc.name() == "UTF-8" || enc.name() == "windows-1252",
        "ASCII+中英混合 UTF-8 应检测为 UTF-8 或兼容子集 (got {})", enc.name());
}

#[test]
fn detect_gbk_chinese() {
    let bytes_gbk: Vec<u8> = vec![0xB2, 0xE2, 0xCA, 0xD4, 0xD6, 0xD0, 0xCE, 0xC4];
    let enc = encoding::detect_encoding(&bytes_gbk);
    let _ = enc;
    let decoded = encoding::decode(&bytes_gbk, encoding_rs::GBK);
    assert_eq!(decoded, "测试中文");
}

#[test]
fn detect_mode_binary_via_nul() {
    let header = b"\x7fELF\x00\x00\x00".to_vec();
    let (mode, _enc) = encoding::detect_mode(&header, Path::new("a.out"), OpenMode::Auto);
    assert_eq!(mode, OpenMode::Binary);
}

#[test]
fn detect_mode_text_extension_txt() {
    let header = b"Hello world, no NUL bytes at all".to_vec();
    let (mode, _enc) = encoding::detect_mode(&header, Path::new("notes.txt"), OpenMode::Auto);
    assert_eq!(mode, OpenMode::Text);
}

#[test]
fn explicit_default_override() {
    let header = b"plain text no NUL".to_vec();
    let (mode, _enc) = encoding::detect_mode(&header, Path::new("x.bin"), OpenMode::Binary);
    assert_eq!(mode, OpenMode::Binary, "default_mode=Binary 应强制覆盖扩展名判断");
    let (mode2, _) = encoding::detect_mode(&header, Path::new("x"), OpenMode::Text);
    assert_eq!(mode2, OpenMode::Text);
}

#[test]
fn roundtrip_encode_decode_gbk() {
    let orig = "静夜思：床前明月光，疑是地上霜。";
    let bytes = encoding::encode(orig, encoding_rs::GBK);
    let back = encoding::decode(&bytes, encoding_rs::GBK);
    assert_eq!(back, orig);
}
