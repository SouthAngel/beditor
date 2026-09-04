use beditor::delta_table::DeltaTable;

const BS: usize = 256 * 1024;

fn dt(count: u64) -> DeltaTable {
    // 完整块（file_size = count * BS，无尾部部分块）
    DeltaTable::new(count, BS, count * BS as u64)
}

#[test]
fn initial_mapping() {
    let dt = dt(10);
    for i in 0..=10u64 {
        assert_eq!(dt.block_start_offset(i, BS), i * BS as u64);
    }
    assert_eq!(dt.total_delta(), 0);
}

#[test]
fn single_growth_propagates() {
    let mut dt = dt(5);
    dt.record_delta(2, 42);
    assert_eq!(dt.block_start_offset(0, BS), 0);
    assert_eq!(dt.block_start_offset(1, BS), BS as u64);
    assert_eq!(dt.block_start_offset(2, BS), 2 * BS as u64);
    assert_eq!(dt.block_start_offset(3, BS), 3 * BS as u64 + 42);
    assert_eq!(dt.block_start_offset(4, BS), 4 * BS as u64 + 42);
    assert_eq!(dt.total_delta(), 42);
}

#[test]
fn multiple_deltas() {
    let mut dt = dt(10);
    dt.record_delta(0, 10);
    dt.record_delta(3, -5);
    assert_eq!(dt.block_start_offset(4, BS), 4 * BS as u64 + 5);
    assert_eq!(dt.total_delta(), 10 + (-5));
}

#[test]
fn insert_block_after() {
    let mut dt = dt(5);
    dt.insert_block_after(2, 0);
    assert_eq!(dt.block_start_offset(0, BS), 0);
    assert_eq!(dt.block_start_offset(2, BS), 2 * BS as u64);
    assert_eq!(dt.block_start_offset(3, BS), 3 * BS as u64);
    assert_eq!(dt.block_start_offset(4, BS), 3 * BS as u64);
    assert_eq!(dt.block_start_offset(5, BS), 4 * BS as u64);
}

#[test]
fn merge_block_adjacent() {
    let mut dt = dt(6);
    dt.record_delta(2, 10);
    dt.record_delta(3, 20);
    dt.merge_block(2, 3);
    assert_eq!(dt.total_delta(), 30);
    assert_eq!(dt.block_start_offset(3, BS), 4 * BS as u64 + 30);
}

#[test]
#[should_panic(expected = "merge_block requires removed == target + 1")]
fn merge_non_adjacent_panics() {
    let mut dt = dt(5);
    dt.merge_block(0, 2);
}

#[test]
fn block_delta_method() {
    let mut dt = dt(5);
    dt.record_delta(2, 42);
    assert_eq!(dt.block_delta(0), 0);
    assert_eq!(dt.block_delta(2), 42);
    assert_eq!(dt.block_delta(4), 0);
}

#[test]
fn partial_last_block_initial_size() {
    // file_size = 3*BS + 100，最后一块只有 100 字节
    let file_size = (3 * BS + 100) as u64;
    let dt = DeltaTable::new(4, BS, file_size);
    println!("file_size={} block_start_offset(3)={} block_delta(3)={}",
        file_size, dt.block_start_offset(3, BS), dt.block_delta(3));
    assert_eq!(dt.block_start_offset(3, BS), (3 * BS) as u64);
    assert_eq!(dt.block_delta(3), 0, "部分块的初始 delta 应为 0");
    assert_eq!(dt.total_delta(), 0);
}

#[test]
fn locate_offset_basic() {
    let dt = dt(10);
    // 块 3 内偏移 50
    let (bid, inner) = dt.locate_offset(3 * BS as u64 + 50);
    assert_eq!(bid, 3);
    assert_eq!(inner, 50);
    // 块 0 起始
    let (bid, inner) = dt.locate_offset(0);
    assert_eq!(bid, 0);
    assert_eq!(inner, 0);
}

#[test]
fn locate_offset_after_growth() {
    let mut dt = dt(10);
    // 块 3 增长 42 字节
    dt.record_delta(3, 42);
    // 块 7 的逻辑起点现在 = 7*BS + 42（前 3 个块后增长 42）
    let (bid, inner) = dt.locate_offset(7 * BS as u64 + 42 + 100);
    assert_eq!(bid, 7);
    assert_eq!(inner, 100);
}

#[test]
fn locate_offset_at_block_boundary() {
    let dt = dt(10);
    // 正好在块 5 起点
    let (bid, inner) = dt.locate_offset(5 * BS as u64);
    assert_eq!(bid, 5);
    assert_eq!(inner, 0);
}
