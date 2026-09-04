use beditor::block::GapBuffer;

#[test]
fn new_empty_then_insert_head() {
    let mut gb = GapBuffer::new(vec![]);
    assert_eq!(gb.len(), 0);
    gb.insert(0, b"hello").unwrap();
    assert_eq!(gb.as_contiguous(), b"hello");
    assert_eq!(gb.len(), 5);
}

#[test]
fn insert_at_all_positions() {
    let mut gb = GapBuffer::new(b"01234".to_vec());
    gb.insert(0, b"A").unwrap();   // A01234
    gb.insert(2, b"B").unwrap();   // A0B1234
    gb.insert(7, b"Z").unwrap();   // A0B1234Z
    assert_eq!(gb.as_contiguous(), b"A0B1234Z");
}

#[test]
fn delete_various() {
    let mut gb = GapBuffer::new(b"ABCDEFGH".to_vec());
    gb.delete(0, 1).unwrap();      // BCDEFGH
    gb.delete(2, 3).unwrap();      // BCGH (删 D,E,F)
    gb.delete(3, 1).unwrap();      // BCG
    assert_eq!(gb.as_contiguous(), b"BCG");
    gb.delete(0, 3).unwrap();
    assert_eq!(gb.len(), 0);
}

#[test]
fn slice() {
    let gb = GapBuffer::new(b"0123456789".to_vec());
    assert_eq!(gb.slice(2..5).unwrap(), b"234");
    assert_eq!(gb.slice(0..10).unwrap(), b"0123456789");
}

#[test]
fn split_at() {
    let mut gb = GapBuffer::new(b"HELLOWORLD".to_vec());
    let right = gb.split_at(5).unwrap();
    assert_eq!(gb.as_contiguous(), b"HELLO");
    assert_eq!(right.as_contiguous(), b"WORLD");
}

#[test]
fn insert_growth_1mb() {
    let mut gb = GapBuffer::new(vec![]);
    let chunk = b"X".repeat(1024);
    for _ in 0..1024 {
        gb.insert(0, &chunk).unwrap();
    }
    assert_eq!(gb.len(), 1024 * 1024);
}

#[test]
fn delete_out_of_bounds_errors() {
    let mut gb = GapBuffer::new(b"012345".to_vec());
    let e = gb.delete(3, 10).unwrap_err();
    assert!(e.to_string().contains("range out of bounds"), "{e}");
}
