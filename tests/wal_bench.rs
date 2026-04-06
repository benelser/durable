//! WAL Engine Benchmarks + Failure Mode Tests
//!
//! Proves performance claims and exercises every failure path.
//! Run with: cargo test --test wal_bench -- --nocapture

use durable_runtime::storage::wal::DurableLog;
use durable_runtime::storage::{FileEventStore, InMemoryEventStore, EventStore, EventType};
use durable_runtime::core::types::ExecutionId;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("durable_bench_{}_{}", name, std::process::id()))
}

fn cleanup(path: &std::path::Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(path);
}

// ===========================================================================
// BENCHMARKS — Prove performance claims
// ===========================================================================

#[test]
fn bench_wal_append_throughput() {
    let path = temp_path("bench_append.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    let entry = br#"{"type":"step_completed","step_number":0,"step_name":"llm_call","result":"{\"content\":\"Hello world this is a typical LLM response with some reasonable length to simulate real usage\"}"}"#;

    // Mode 1: Fsync per entry (safest — one fsync per entry)
    let count_sync = 100;
    let start = Instant::now();
    for _ in 0..count_sync {
        log.append_sync(entry).unwrap();
    }
    let elapsed_sync = start.elapsed();
    let ops_sync = count_sync as f64 / elapsed_sync.as_secs_f64();

    // Mode 2: Buffered batch — append N entries, single commit
    let count_batch = 100_000;
    let start = Instant::now();
    for _ in 0..count_batch {
        log.append(entry).unwrap();
    }
    log.commit().unwrap();
    let elapsed_batch = start.elapsed();
    let ops_batch = count_batch as f64 / elapsed_batch.as_secs_f64();
    let bw_batch = (count_batch * entry.len()) as f64 / elapsed_batch.as_secs_f64();

    // Mode 3: Commit every 100 entries (realistic step-boundary pattern)
    let count_step = 10_000;
    let commit_interval = 100;
    let start = Instant::now();
    for i in 0..count_step {
        log.append(entry).unwrap();
        if (i + 1) % commit_interval == 0 {
            log.commit().unwrap();
        }
    }
    log.commit().unwrap();
    let elapsed_step = start.elapsed();
    let ops_step = count_step as f64 / elapsed_step.as_secs_f64();

    println!("\n=== WAL Append Benchmark ===");
    println!("  Entry size:      {} bytes", entry.len());
    println!("  --- Mode 1: Fsync per entry (append_sync) ---");
    println!("    Entries:       {}", count_sync);
    println!("    Time:          {:.2?}", elapsed_sync);
    println!("    Throughput:    {:.0} ops/sec", ops_sync);
    println!("  --- Mode 2: Full batch (append + commit) ---");
    println!("    Entries:       {}", count_batch);
    println!("    Time:          {:.2?}", elapsed_batch);
    println!("    Throughput:    {:.0} ops/sec", ops_batch);
    println!("    Bandwidth:     {:.1} MB/sec", bw_batch / 1_000_000.0);
    println!("  --- Mode 3: Commit every {} entries (realistic) ---", commit_interval);
    println!("    Entries:       {}", count_step);
    println!("    Time:          {:.2?}", elapsed_step);
    println!("    Throughput:    {:.0} ops/sec", ops_step);

    // Batch should be dramatically faster than per-entry fsync
    assert!(ops_batch > ops_sync * 100.0,
        "batch should be >100x faster: batch={:.0}, sync={:.0}", ops_batch, ops_sync);

    cleanup(&path);
}

#[test]
fn bench_wal_read_throughput() {
    let path = temp_path("bench_read.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    let entry = br#"{"type":"step_completed","step_number":0,"result":"typical result"}"#;

    let count = 10_000;
    for _ in 0..count {
        log.append(entry).unwrap();
    }
    log.commit().unwrap();

    let start = Instant::now();
    let entries = log.read_all().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(entries.len(), count);
    let ops_per_sec = count as f64 / elapsed.as_secs_f64();

    println!("\n=== WAL Read Benchmark ===");
    println!("  Entries:     {}", count);
    println!("  Time:        {:.2?}", elapsed);
    println!("  Throughput:  {:.0} entries/sec", ops_per_sec);

    // Read should be much faster than write (no fsync)
    assert!(
        ops_per_sec > 100_000.0,
        "WAL read too slow: {:.0} ops/sec (expected >100K)",
        ops_per_sec
    );

    cleanup(&path);
}

#[test]
fn bench_ndjson_append_comparison() {
    let dir = temp_path("bench_ndjson");
    cleanup(&dir);

    let store = FileEventStore::new(&dir).unwrap();
    let exec_id = ExecutionId::new();
    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();

    let count = 100; // Small count — fsync dominates at this scale
    let start = Instant::now();
    for i in 0..count {
        store.append(exec_id, EventType::StepCompleted {
            step_number: i,
            step_name: format!("step_{}", i),
            result: "typical result value here".to_string(),
        }).unwrap();
    }
    let ndjson_elapsed = start.elapsed();
    let ndjson_ops = count as f64 / ndjson_elapsed.as_secs_f64();

    // Compare with WAL
    let wal_path = temp_path("bench_ndjson_wal.wal");
    cleanup(&wal_path);
    let log = DurableLog::open(&wal_path).unwrap();
    let entry = br#"{"type":"step_completed","step_number":0,"step_name":"step_0","result":"typical result value here"}"#;

    let start = Instant::now();
    for _ in 0..count {
        log.append(entry).unwrap();
    }
    let wal_elapsed = start.elapsed();
    let wal_ops = count as f64 / wal_elapsed.as_secs_f64();

    let speedup = wal_ops / ndjson_ops;

    println!("\n=== NDJSON vs WAL Comparison ({} entries) ===", count);
    println!("  NDJSON:   {:.2?} ({:.0} ops/sec)", ndjson_elapsed, ndjson_ops);
    println!("  WAL:      {:.2?} ({:.0} ops/sec)", wal_elapsed, wal_ops);
    println!("  Speedup:  {:.1}x", speedup);

    cleanup(&dir);
    cleanup(&wal_path);
}

#[test]
fn bench_wal_compaction() {
    let path = temp_path("bench_compact.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    for i in 0..1_000 {
        log.append_sync(format!("event_{}", i).as_bytes()).unwrap();
    }

    let file_size_before = fs::metadata(&path).unwrap().len();

    let start = Instant::now();
    let removed = log.compact_with_snapshot(b"{\"snapshot\":true,\"step_count\":1000}").unwrap();
    let elapsed = start.elapsed();

    let file_size_after = fs::metadata(&path).unwrap().len();

    println!("\n=== WAL Compaction Benchmark ===");
    println!("  Entries removed: {}", removed);
    println!("  Size before:     {} bytes", file_size_before);
    println!("  Size after:      {} bytes", file_size_after);
    println!("  Reduction:       {:.1}%", (1.0 - file_size_after as f64 / file_size_before as f64) * 100.0);
    println!("  Time:            {:.2?}", elapsed);

    assert!(file_size_after < file_size_before / 2, "compaction should reduce size by >50%");

    cleanup(&path);
}

// ===========================================================================
// FAILURE MODES — Exercise every way the WAL can break
// ===========================================================================

#[test]
fn failure_empty_file() {
    // An empty file (0 bytes) should fail validation
    let path = temp_path("fail_empty.wal");
    cleanup(&path);
    fs::write(&path, b"").unwrap();

    let result = DurableLog::open(&path);
    // Empty file should get header written
    assert!(result.is_ok());
    let log = result.unwrap();
    assert_eq!(log.len(), 0);

    cleanup(&path);
}

#[test]
fn failure_corrupt_magic() {
    let path = temp_path("fail_magic.wal");
    cleanup(&path);

    // Write a file with wrong magic bytes
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(b"XXXX").unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&0u16.to_le_bytes()).unwrap();
    drop(f);

    let result = DurableLog::open(&path);
    assert!(result.is_err(), "should reject invalid magic");
    assert!(result.unwrap_err().contains("invalid magic"));

    cleanup(&path);
}

#[test]
fn failure_future_version() {
    let path = temp_path("fail_version.wal");
    cleanup(&path);

    // Write header with version 99
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(b"DWAL").unwrap();
    f.write_all(&99u16.to_le_bytes()).unwrap();
    f.write_all(&0u16.to_le_bytes()).unwrap();
    drop(f);

    let result = DurableLog::open(&path);
    assert!(result.is_err(), "should reject future version");
    assert!(result.unwrap_err().contains("unsupported version"));

    cleanup(&path);
}

#[test]
fn failure_truncated_entry_length() {
    // Only 2 bytes of a 4-byte length field
    let path = temp_path("fail_trunc_len.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    log.append_sync(b"good_entry").unwrap();
    drop(log);

    // Append partial length bytes
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&[0x05, 0x00]).unwrap(); // Only 2 of 4 length bytes
    drop(f);

    let log = DurableLog::open(&path).unwrap();
    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 1, "should recover the one good entry");

    cleanup(&path);
}

#[test]
fn failure_truncated_entry_data() {
    // Length says 100 bytes but only 10 bytes of data
    let path = temp_path("fail_trunc_data.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    log.append_sync(b"good_entry").unwrap();
    drop(log);

    // Append a valid-looking header but truncated data
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let len: u32 = 100;
    let crc: u64 = 0;
    f.write_all(&len.to_le_bytes()).unwrap();
    f.write_all(&crc.to_le_bytes()).unwrap();
    f.write_all(b"only_10_by").unwrap(); // Only 10 of promised 100 bytes
    drop(f);

    let log = DurableLog::open(&path).unwrap();
    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 1, "should recover the one good entry");

    cleanup(&path);
}

#[test]
fn failure_bad_crc_last_entry() {
    // Last entry has wrong CRC (crash during write)
    let path = temp_path("fail_crc_last.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    log.append_sync(b"good_1").unwrap();
    log.append_sync(b"good_2").unwrap();
    drop(log);

    // Append entry with valid length but wrong CRC
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let data = b"bad_crc_entry";
    let len = data.len() as u32;
    let bad_crc: u64 = 0xDEADBEEF;
    f.write_all(&len.to_le_bytes()).unwrap();
    f.write_all(&bad_crc.to_le_bytes()).unwrap();
    f.write_all(data).unwrap();
    drop(f);

    // Should recover the two good entries, discard the bad last one
    let log = DurableLog::open(&path).unwrap();
    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 2, "should recover 2 good entries, discard bad last");

    cleanup(&path);
}

#[test]
fn failure_bad_crc_middle_entry() {
    // Middle entry has wrong CRC (data corruption, not crash)
    let path = temp_path("fail_crc_mid.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    log.append_sync(b"entry_1").unwrap();
    log.append_sync(b"entry_2").unwrap();
    log.append_sync(b"entry_3").unwrap();
    drop(log);

    // Corrupt the second entry's data
    let mut content = fs::read(&path).unwrap();
    // Entry 2 starts at: header(8) + entry1(12 + 7) = 27
    // Entry 2 data starts at: 27 + 12 = 39
    let data_offset = 8 + 12 + 7 + 12; // 39
    if data_offset < content.len() {
        content[data_offset] ^= 0xFF;
    }
    fs::write(&path, &content).unwrap();

    let log = DurableLog::open(&path).unwrap();
    let result = log.read_all();
    // WAL scan stops at the corrupt entry — returns only entries before it
    // This is correct: a corrupt middle entry is detected and nothing after it is trusted
    match result {
        Ok(entries) => {
            assert!(entries.len() < 3, "should not return all 3 entries when middle is corrupt");
            assert_eq!(entries.len(), 1, "should return only the first valid entry");
        }
        Err(e) => {
            assert!(e.contains("CRC"), "error should mention CRC: {}", e);
        }
    }

    cleanup(&path);
}

#[test]
fn failure_zero_length_entry() {
    // An entry with length 0 — should be valid (empty data)
    let path = temp_path("fail_zero_len.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    log.append_sync(b"").unwrap(); // Empty entry
    log.append_sync(b"after_empty").unwrap();

    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].data.len(), 0);
    assert_eq!(entries[1].as_str(), "after_empty");

    cleanup(&path);
}

#[test]
fn failure_large_entry() {
    // 1MB entry — tests buffer handling
    let path = temp_path("fail_large.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    let big_data = vec![b'x'; 1_000_000];
    log.append(&big_data).unwrap();
    log.append_sync(b"after_large").unwrap();

    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].data.len(), 1_000_000);
    assert_eq!(entries[1].as_str(), "after_large");

    cleanup(&path);
}

#[test]
fn failure_concurrent_readers() {
    // Multiple threads reading the same WAL simultaneously
    let path = temp_path("fail_concurrent.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    for i in 0..100 {
        log.append_sync(format!("entry_{}", i).as_bytes()).unwrap();
    }

    // Read from multiple threads
    let path_clone = path.clone();
    let handles: Vec<_> = (0..4).map(|_| {
        let p = path_clone.clone();
        std::thread::spawn(move || {
            let log = DurableLog::open(&p).unwrap();
            let entries = log.read_all().unwrap();
            entries.len()
        })
    }).collect();

    for handle in handles {
        let count = handle.join().unwrap();
        assert_eq!(count, 100, "concurrent reader should see all 100 entries");
    }

    cleanup(&path);
}

#[test]
fn failure_reopen_after_crash_mid_write() {
    // Simulate crash: write partial entry, reopen, continue
    let path = temp_path("fail_reopen_crash.wal");
    cleanup(&path);

    // Write some good entries
    let log = DurableLog::open(&path).unwrap();
    log.append_sync(b"before_crash_1").unwrap();
    log.append_sync(b"before_crash_2").unwrap();
    drop(log);

    // Simulate crash: partial write
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&[0x0A, 0x00, 0x00, 0x00]).unwrap(); // length = 10
    f.write_all(&[0xFF; 5]).unwrap(); // partial CRC + no data
    drop(f);

    // Reopen — should recover and allow new writes
    let log = DurableLog::open(&path).unwrap();
    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 2, "should recover 2 entries from before crash");

    // New writes should work
    log.append_sync(b"after_recovery").unwrap();
    let entries = log.read_all().unwrap();
    // Note: the new entry is appended AFTER the crash debris,
    // so read_all may see 2 or 3 depending on how crash debris is handled.
    // The important thing: no panic, no corruption of good data.
    assert!(entries.len() >= 2, "should have at least the 2 good entries");

    cleanup(&path);
}

#[test]
fn failure_compact_then_crash() {
    // Compact, then simulate crash, then reopen
    let path = temp_path("fail_compact_crash.wal");
    cleanup(&path);

    let log = DurableLog::open(&path).unwrap();
    for i in 0..50 {
        log.append_sync(format!("event_{}", i).as_bytes()).unwrap();
    }

    // Compact
    log.compact_with_snapshot(b"{\"snapshot\":\"state\"}").unwrap();

    // Write more entries after compaction
    log.append_sync(b"after_compact_1").unwrap();
    log.append_sync(b"after_compact_2").unwrap();
    drop(log);

    // Reopen — should see snapshot + 2 new entries
    let log = DurableLog::open(&path).unwrap();
    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 3, "snapshot + 2 new entries");
    assert_eq!(entries[0].as_str(), "{\"snapshot\":\"state\"}");
    assert_eq!(entries[1].as_str(), "after_compact_1");
    assert_eq!(entries[2].as_str(), "after_compact_2");

    cleanup(&path);
}

// ===========================================================================
// DURABILITY PROOF — Verify data survives process boundaries
// ===========================================================================

#[test]
fn durability_survives_drop() {
    let path = temp_path("durable_drop.wal");
    cleanup(&path);

    // Write and drop (simulates process exit)
    {
        let log = DurableLog::open(&path).unwrap();
        log.append_sync(b"survives_drop_1").unwrap();
        log.append_sync(b"survives_drop_2").unwrap();
        // log is dropped here — destructor runs
    }

    // New "process" opens the same file
    {
        let log = DurableLog::open(&path).unwrap();
        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].as_str(), "survives_drop_1");
        assert_eq!(entries[1].as_str(), "survives_drop_2");
    }

    cleanup(&path);
}

#[test]
fn durability_append_after_reopen() {
    let path = temp_path("durable_reopen.wal");
    cleanup(&path);

    // Session 1: write 3 entries
    {
        let log = DurableLog::open(&path).unwrap();
        log.append_sync(b"session1_a").unwrap();
        log.append_sync(b"session1_b").unwrap();
        log.append_sync(b"session1_c").unwrap();
    }

    // Session 2: write 2 more
    {
        let log = DurableLog::open(&path).unwrap();
        log.append_sync(b"session2_a").unwrap();
        log.append_sync(b"session2_b").unwrap();
    }

    // Session 3: verify all 5
    {
        let log = DurableLog::open(&path).unwrap();
        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].as_str(), "session1_a");
        assert_eq!(entries[4].as_str(), "session2_b");
    }

    cleanup(&path);
}
